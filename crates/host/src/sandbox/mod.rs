//! Out-of-process plugin hosting ("sandbox").
//!
//! [`SandboxedPlugin`] is an `aitrack_dsp::Processor` whose plugin lives in
//! a separate `aitrack-plugin-host` process: a crash there silences one
//! node instead of taking the DAW down. The audio path is a shared-memory
//! region with a same-cycle handoff (see [`protocol`]), so the sandbox adds
//! no buffer of latency, only the IPC wake-up cost.
//!
//! Status: one plugin per host process; audio, events, parameters (both
//! ways), latency, state and editor open/close work. Not yet: grouping
//! several plugins per process, sidechain, MMCSS / audio-workgroup joining
//! of the sandbox RT thread, crash restart with state recovery.

pub mod protocol;
pub mod server;
pub mod shm;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aitrack_dsp::{Category, PluginInfo, ProcessContext, Processor};
use parking_lot::Mutex;

use crate::error::{HostError, Result};
use crate::info::HostedPluginInfo;
use crate::params::{PARAM_EDIT_QUEUE, ParamEdit, ParamTable, PendingParams};
use crate::strings::intern;
use protocol::*;
use shm::{SharedMemory, Signal};

pub const HOST_EXE: &str = if cfg!(windows) { "aitrack-plugin-host.exe" } else { "aitrack-plugin-host" };

/// Suppress the OS crash dialog in helper processes (Windows Error
/// Reporting would otherwise block a crashed scanner/host until the timeout).
pub fn disable_crash_dialogs() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode};
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
}

fn unique_names() -> (String, String, String) {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    #[cfg(windows)]
    {
        let base = format!("Local\\AITrack-sbx-{pid}-{n}");
        (format!("{base}-shm"), format!("{base}-req"), format!("{base}-done"))
    }
    #[cfg(not(windows))]
    {
        // macOS limits POSIX names to 31 characters.
        (format!("/aitk{pid}.{n}m"), format!("/aitk{pid}.{n}q"), format!("/aitk{pid}.{n}d"))
    }
}

struct Control {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<Response>,
}

impl Control {
    fn call(&mut self, req: &Request, timeout: Duration) -> Result<Response> {
        let line = serde_json::to_string(req)?;
        writeln!(self.stdin, "{line}").and_then(|_| self.stdin.flush()).map_err(|e| HostError::Sandbox(format!("sandbox pipe closed: {e}")))?;
        match self.rx.recv_timeout(timeout) {
            Ok(Response::Error { message }) => Err(HostError::Sandbox(message)),
            Ok(r) => Ok(r),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(HostError::Sandbox("sandbox did not answer in time".into())),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(HostError::Sandbox("sandbox process exited".into())),
        }
    }
}

/// A plugin hosted in its own `aitrack-plugin-host` process.
pub struct SandboxedPlugin {
    info: PluginInfo,
    hosted: HostedPluginInfo,
    control: Mutex<Control>,
    params: ParamTable,
    pending: PendingParams,
    region: Option<Region>,
    request: Option<Signal>,
    done: Option<Signal>,
    seq: u64,
    budget: Option<Duration>,
    sample_rate: f64,
    in_ch: usize,
    out_ch: usize,
    edits_tx: rtrb::Producer<ParamEdit>,
    edits_rx: Option<rtrb::Consumer<ParamEdit>>,
    latency: u32,
    late_blocks: u64,
    crashed: bool,
}

impl SandboxedPlugin {
    /// Spawn `aitrack-plugin-host` from next to the current executable.
    pub fn spawn(info: &HostedPluginInfo) -> Result<Self> {
        let exe = crate::scan::sibling_exe(HOST_EXE).ok_or_else(|| HostError::Sandbox(format!("{HOST_EXE} not found")))?;
        Self::spawn_with(&exe, info)
    }

    pub fn spawn_with(exe: &Path, info: &HostedPluginInfo) -> Result<Self> {
        let mut child = Command::new(exe).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| HostError::Sandbox("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| HostError::Sandbox("no stdout".into()))?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("aitrack-sandbox-reader".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                // Anything without our prefix is plugin chatter.
                if let Some(json) = line.strip_prefix(CONTROL_PREFIX) {
                    match serde_json::from_str::<Response>(json) {
                        Ok(r) => {
                            if tx.send(r).is_err() {
                                break;
                            }
                        }
                        Err(e) => log::warn!("bad sandbox message: {e}"),
                    }
                }
            }
        })?;
        let mut control = Control { child, stdin, rx };
        let (hosted, wire, values) = match control.call(&Request::Load { info: info.clone() }, Duration::from_secs(30)) {
            Ok(Response::Loaded { info, params, values }) => (info, params, values),
            Ok(other) => return Err(HostError::Sandbox(format!("unexpected reply {other:?}"))),
            Err(e) => {
                let _ = control.child.kill();
                return Err(e);
            }
        };
        let params: Vec<_> = wire.iter().map(WireParam::to_info).collect();
        let info = PluginInfo {
            id: intern(&hosted.stable_id()),
            name: intern(&hosted.name),
            vendor: intern(&hosted.vendor),
            category: if hosted.is_instrument { Category::Instrument } else { Category::Effect },
            tags: &[],
            params: params.clone(),
            inputs: hosted.inputs,
            outputs: hosted.outputs,
            has_sidechain: false,
        };
        let table = ParamTable::new(params);
        for (id, v) in values {
            table.set(id, v);
        }
        let pending = PendingParams::new(table.len());
        let (edits_tx, edits_rx) = rtrb::RingBuffer::new(PARAM_EDIT_QUEUE);
        Ok(Self {
            info,
            hosted,
            control: Mutex::new(control),
            params: table,
            pending,
            region: None,
            request: None,
            done: None,
            seq: 0,
            budget: None,
            sample_rate: 48_000.0,
            in_ch: 2,
            out_ch: 2,
            edits_tx,
            edits_rx: Some(edits_rx),
            latency: 0,
            late_blocks: 0,
            crashed: false,
        })
    }

    pub fn hosted_info(&self) -> &HostedPluginInfo {
        &self.hosted
    }

    /// Override the per-block wait budget (default: 70 % of the block).
    pub fn set_block_budget(&mut self, budget: Option<Duration>) {
        self.budget = budget;
    }

    /// Blocks that came back too late (output silenced).
    pub fn late_blocks(&self) -> u64 {
        self.late_blocks
    }

    pub fn is_crashed(&self) -> bool {
        self.crashed
    }

    pub fn take_param_edits(&mut self) -> Option<rtrb::Consumer<ParamEdit>> {
        self.edits_rx.take()
    }

    /// The editor opens as a top-level window of the sandbox process.
    pub fn open_editor(&self) -> Result<()> {
        self.control.lock().call(&Request::OpenEditor, Duration::from_secs(10)).map(|_| ())
    }

    pub fn close_editor(&self) -> Result<()> {
        self.control.lock().call(&Request::CloseEditor, Duration::from_secs(10)).map(|_| ())
    }

    /// Non-blocking liveness check of the host process.
    pub fn is_alive(&mut self) -> bool {
        !self.crashed && matches!(self.control.get_mut().child.try_wait(), Ok(None))
    }

    fn setup_region(&mut self, sample_rate: f64, max_frames: usize) -> Result<()> {
        let (shm_name, req_name, done_name) = unique_names();
        let layout = Layout::new(max_frames, self.in_ch, self.out_ch);
        let shm = SharedMemory::create(&shm_name, layout.size)?;
        let region = Region { shm, layout };
        region.init();
        let request = Signal::create(&req_name)?;
        let done = Signal::create(&done_name)?;
        let req = Request::Prepare {
            sample_rate,
            max_frames: max_frames as u32,
            in_channels: self.in_ch as u32,
            out_channels: self.out_ch as u32,
            shm: shm_name,
            request: req_name,
            done: done_name,
        };
        match self.control.lock().call(&req, Duration::from_secs(30))? {
            Response::Prepared { latency } => self.latency = latency,
            other => return Err(HostError::Sandbox(format!("unexpected reply {other:?}"))),
        }
        self.region = Some(region);
        self.request = Some(request);
        self.done = Some(done);
        self.seq = 0;
        self.sample_rate = sample_rate;
        Ok(())
    }
}

impl Processor for SandboxedPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
        if self.crashed {
            return;
        }
        // The old region is released only after the sandbox switched to
        // the new one.
        if let Err(e) = self.setup_region(sample_rate, max_frames.max(1)) {
            log::error!("sandbox prepare for '{}': {e}", self.info.name);
            self.region = None;
        }
    }

    fn reset(&mut self) {
        self.pending.drain(|_, _| {});
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let (Some(region), Some(request), Some(done)) = (&self.region, &self.request, &self.done) else {
            ctx.buffer.clear();
            return;
        };
        if self.crashed {
            ctx.buffer.clear();
            return;
        }
        let frames = ctx.buffer.frames().min(region.layout.max_frames);
        let h = region.header();
        if h.done_seq.load(Ordering::Acquire) != self.seq {
            // Still busy with a late block: never overwrite buffers the
            // sandbox may be reading.
            self.late_blocks += 1;
            ctx.buffer.clear();
            return;
        }

        // ── Inputs ──
        let src_ch = ctx.buffer.channels();
        for ch in 0..self.in_ch {
            let dst = unsafe { &mut region.audio_in(ch)[..frames] };
            if src_ch == 0 {
                dst.fill(0.0);
            } else {
                dst.copy_from_slice(&ctx.buffer.channel(ch.min(src_ch - 1))[..frames]);
            }
        }
        let shm_events = unsafe { region.events() };
        let mut n = 0usize;
        let infos = self.params.infos();
        self.pending.drain(|index, v| {
            if n < MAX_EVENTS {
                shm_events[n] =
                    encode_event(&aitrack_dsp::Event { offset: 0, kind: aitrack_dsp::EventKind::Param { id: infos[index].id, value: v } });
                n += 1;
            }
        });
        for e in ctx.events {
            if n >= MAX_EVENTS {
                break;
            }
            if let aitrack_dsp::EventKind::Param { id, value } = e.kind {
                self.params.set(id, value);
            }
            shm_events[n] = encode_event(e);
            n += 1;
        }
        let seq = self.seq + 1;
        unsafe {
            let hm = region.header_mut();
            hm.frames = frames as u32;
            hm.n_events = n as u32;
            hm.sample_rate = ctx.sample_rate;
            hm.transport = ShmTransport::from(ctx.transport);
        }
        self.seq = seq;
        region.header().seq.store(seq, Ordering::Release);
        request.post();

        // ── Same-cycle wait ──
        let budget =
            self.budget.unwrap_or_else(|| Duration::from_secs_f64(frames as f64 / ctx.sample_rate.max(1.0) * 0.7)).max(Duration::from_micros(500));
        let start = Instant::now();
        let on_time = loop {
            if h.done_seq.load(Ordering::Acquire) == seq {
                break true;
            }
            let elapsed = start.elapsed();
            if elapsed >= budget {
                break false;
            }
            done.wait(budget - elapsed);
        };
        if !on_time {
            self.late_blocks += 1;
            ctx.buffer.clear();
            if self.late_blocks.is_multiple_of(64)
                && let Ok(Some(_)) = self.control.get_mut().child.try_wait()
            {
                log::error!("sandbox for '{}' exited", self.info.name);
                self.crashed = true;
            }
            return;
        }

        // ── Outputs ──
        for ch in 0..ctx.buffer.channels() {
            let src = unsafe { &region.audio_out(ch.min(self.out_ch.max(1) - 1))[..frames] };
            ctx.buffer.channel_mut(ch)[..frames].copy_from_slice(src);
        }
        crate::io::sanitize(ctx.buffer);
        let n_edits = (h.n_edits as usize).min(MAX_EDITS);
        let edits = unsafe { region.edits() };
        for e in &edits[..n_edits] {
            let edit = decode_edit(e);
            if edit.value.is_finite() {
                self.params.set(edit.id, edit.value);
            }
            let _ = self.edits_tx.push(edit);
        }
        self.latency = h.latency.load(Ordering::Relaxed);
        if h.status.load(Ordering::Relaxed) != 0 {
            self.crashed = true;
        }
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        if let Some(i) = self.params.index_of(id) {
            let v = normalized.clamp(0.0, 1.0);
            self.params.set(id, v);
            self.pending.push(i, v);
        }
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn save_state(&self) -> Vec<u8> {
        match self.control.lock().call(&Request::SaveState, Duration::from_secs(10)) {
            Ok(Response::State { data }) => data,
            other => {
                log::error!("sandbox save_state: {other:?}");
                Vec::new()
            }
        }
    }

    fn load_state(&mut self, data: &[u8]) {
        match self.control.lock().call(&Request::LoadState { data: data.to_vec() }, Duration::from_secs(10)) {
            Ok(Response::Loaded { values, .. }) => {
                for (id, v) in values {
                    self.params.set(id, v);
                }
            }
            other => log::error!("sandbox load_state: {other:?}"),
        }
    }
}

impl Drop for SandboxedPlugin {
    fn drop(&mut self) {
        let c = self.control.get_mut();
        let _ = c.call(&Request::Shutdown, Duration::from_secs(2));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = c.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = c.child.kill();
        let _ = c.child.wait();
    }
}
