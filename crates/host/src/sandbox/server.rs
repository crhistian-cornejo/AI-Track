//! The `aitrack-plugin-host` process: loads one plugin in-process, serves
//! control requests on its main thread and processes blocks on an RT
//! thread driven by the shared-memory handoff.

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use aitrack_dsp::{AudioBuffer, Event, ProcessContext, Processor};
use parking_lot::Mutex;

use super::protocol::*;
use super::shm::{SharedMemory, Signal};
use crate::params::ParamEdit;
use crate::plugin::{HostedPlugin, PluginController};

type Shared = Arc<Mutex<Option<HostedPlugin>>>;

struct Rt {
    stop: Arc<AtomicBool>,
    request: Arc<Signal>,
    handle: JoinHandle<Option<rtrb::Consumer<ParamEdit>>>,
}

impl Rt {
    fn stop(self) -> Option<rtrb::Consumer<ParamEdit>> {
        self.stop.store(true, Ordering::Release);
        self.request.post();
        self.handle.join().ok().flatten()
    }
}

fn respond(r: &Response) {
    let json = serde_json::to_string(r).unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"serialise\"}".into());
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "\n{CONTROL_PREFIX}{json}");
    let _ = out.flush();
}

fn loaded_reply(p: &HostedPlugin) -> Response {
    let params = p.info().params.iter().map(WireParam::from_info).collect();
    let values = p.info().params.iter().map(|q| (q.id, p.get_param(q.id))).collect();
    Response::Loaded { info: p.hosted_info().clone(), params, values }
}

/// Entry point of `aitrack-plugin-host`. Runs until the parent sends
/// `Shutdown` or closes stdin (parent died).
pub fn host_main() -> i32 {
    super::disable_crash_dialogs();
    // Plugin editors on Windows (JUCE, VSTGUI) expect an OLE apartment on
    // their thread for drag and drop and the clipboard; GPUI does this for
    // the app, the sandbox has to do it itself. Never uninitialised: the
    // process exits right after the loop.
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::System::Ole::OleInitialize(None);
    }
    let (tx, rx) = mpsc::channel::<Option<Request>>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Request>(&line) {
                Ok(r) => {
                    if tx.send(Some(r)).is_err() {
                        return;
                    }
                }
                Err(e) => respond(&Response::Error { message: format!("bad request: {e}") }),
            }
        }
        let _ = tx.send(None);
    });

    let plugin: Shared = Arc::new(Mutex::new(None));
    let mut controller: Option<PluginController> = None;
    let mut edits: Option<rtrb::Consumer<ParamEdit>> = None;
    let mut rt: Option<Rt> = None;

    // The parent's watchdog kills this process when the heartbeat stops
    // (an editor stuck inside one AppKit/Win32 call never returns here).
    let mut alive = std::time::Instant::now();
    respond(&Response::Alive);
    loop {
        if alive.elapsed() >= HEARTBEAT {
            respond(&Response::Alive);
            alive = std::time::Instant::now();
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Some(req)) => {
                let shutdown = matches!(req, Request::Shutdown);
                let freeze = matches!(req, Request::Freeze);
                let reply = handle(req, &plugin, &mut controller, &mut edits, &mut rt);
                respond(&reply);
                if shutdown {
                    break;
                }
                if freeze {
                    loop {
                        std::thread::sleep(Duration::from_secs(3600));
                    }
                }
            }
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(c) = &controller {
            c.idle();
        }
        crate::editor::pump_messages();
    }

    if let Some(r) = rt.take() {
        r.stop();
    }
    if let Some(c) = &controller {
        c.close_editor();
    }
    // Drop the plugin here, on its main thread.
    drop(plugin.lock().take());
    0
}

fn handle(
    req: Request,
    plugin: &Shared,
    controller: &mut Option<PluginController>,
    edits: &mut Option<rtrb::Consumer<ParamEdit>>,
    rt: &mut Option<Rt>,
) -> Response {
    let err = |m: String| Response::Error { message: m };
    match req {
        Request::Load { info } => match HostedPlugin::load(&info) {
            Ok(mut p) => {
                *edits = p.take_param_edits();
                *controller = Some(p.controller());
                let reply = loaded_reply(&p);
                *plugin.lock() = Some(p);
                reply
            }
            Err(e) => err(e.to_string()),
        },
        Request::Prepare { sample_rate, max_frames, in_channels, out_channels, shm, request, done } => {
            if let Some(r) = rt.take()
                && let Some(e) = r.stop()
            {
                *edits = Some(e);
            }
            let layout = Layout::new(max_frames as usize, in_channels as usize, out_channels as usize);
            let region = match SharedMemory::open(&shm, layout.size) {
                Ok(shm) => Region { shm, layout },
                Err(e) => return err(e.to_string()),
            };
            if !region.validate() {
                return err("shared memory layout mismatch".into());
            }
            let (request, done) = match (Signal::open(&request), Signal::open(&done)) {
                (Ok(a), Ok(b)) => (Arc::new(a), b),
                (Err(e), _) | (_, Err(e)) => return err(e.to_string()),
            };
            let latency = {
                let mut g = plugin.lock();
                let Some(p) = g.as_mut() else { return err("no plugin loaded".into()) };
                p.prepare(sample_rate, max_frames as usize);
                if !p.is_prepared() {
                    return err("plugin failed to activate".into());
                }
                p.latency()
            };
            region.header().latency.store(latency, Ordering::Relaxed);
            let stop = Arc::new(AtomicBool::new(false));
            let handle = {
                let (stop, request, plugin) = (stop.clone(), request.clone(), plugin.clone());
                let consumer = edits.take();
                std::thread::Builder::new()
                    .name("aitrack-sandbox-rt".into())
                    .spawn(move || rt_loop(plugin, region, request, done, stop, consumer, sample_rate))
            };
            match handle {
                Ok(handle) => {
                    *rt = Some(Rt { stop, request, handle });
                    Response::Prepared { latency }
                }
                Err(e) => err(e.to_string()),
            }
        }
        Request::SaveState => match controller.as_ref().map(|c| c.save_state()) {
            Some(Ok(data)) => Response::State { data },
            Some(Err(e)) => err(e.to_string()),
            None => err("no plugin loaded".into()),
        },
        Request::LoadState { data } => match controller.as_ref().map(|c| c.load_state(&data)) {
            Some(Ok(())) => match plugin.lock().as_ref() {
                Some(p) => loaded_reply(p),
                None => err("no plugin loaded".into()),
            },
            Some(Err(e)) => err(e.to_string()),
            None => err("no plugin loaded".into()),
        },
        Request::OpenEditor => match controller.as_ref().map(|c| c.open_editor()) {
            Some(Ok(())) => Response::Ok,
            Some(Err(e)) => err(e.to_string()),
            None => err("no plugin loaded".into()),
        },
        Request::CloseEditor => {
            if let Some(c) = controller.as_ref() {
                c.close_editor();
            }
            Response::Ok
        }
        Request::Shutdown | Request::Freeze => Response::Ok,
    }
}

fn rt_loop(
    plugin: Shared,
    region: Region,
    request: Arc<Signal>,
    done: Signal,
    stop: Arc<AtomicBool>,
    mut edits: Option<rtrb::Consumer<ParamEdit>>,
    sample_rate: f64,
) -> Option<rtrb::Consumer<ParamEdit>> {
    aitrack_dsp::disable_denormals();
    let l = region.layout;
    let channels = l.in_channels.max(l.out_channels).max(1);
    let mut buffer = AudioBuffer::new(channels, l.max_frames);
    let mut events: Vec<Event> = Vec::with_capacity(MAX_EVENTS);
    // Start from the last *completed* block: the DAW may already have
    // published the next request before this thread got scheduled.
    let mut last = region.header().done_seq.load(Ordering::Acquire);
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if !request.wait(Duration::from_millis(100)) {
            continue;
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        let h = region.header();
        let seq = h.seq.load(Ordering::Acquire);
        if seq == last {
            continue;
        }
        last = seq;
        let frames = (h.frames as usize).min(l.max_frames);
        buffer.set_frames(frames);
        for ch in 0..buffer.channels() {
            let dst = buffer.channel_mut(ch);
            if ch < l.in_channels {
                dst.copy_from_slice(unsafe { &region.audio_in(ch)[..frames] });
            } else {
                dst.fill(0.0);
            }
        }
        events.clear();
        let shm_events = unsafe { region.events() };
        for e in &shm_events[..(h.n_events as usize).min(MAX_EVENTS)] {
            if let Some(ev) = decode_event(e)
                && (ev.offset as usize) < frames.max(1)
            {
                events.push(ev);
            }
        }
        let transport = h.transport.to();
        let sr = if h.sample_rate > 0.0 { h.sample_rate } else { sample_rate };
        let mut status = 0;
        let mut latency = h.latency.load(Ordering::Relaxed);
        match plugin.try_lock() {
            Some(mut g) => match g.as_mut() {
                Some(p) => {
                    let mut ctx = ProcessContext { sample_rate: sr, buffer: &mut buffer, sidechain: None, events: &events, transport: &transport };
                    p.process(&mut ctx);
                    if p.is_crashed() {
                        status = 1;
                    }
                    latency = p.latency();
                }
                None => buffer.clear(),
            },
            // The main thread is re-preparing or saving: silence this block.
            None => buffer.clear(),
        }
        for ch in 0..l.out_channels {
            let src = buffer.channel(ch.min(buffer.channels() - 1));
            unsafe { region.audio_out(ch)[..frames].copy_from_slice(src) };
        }
        let mut n = 0usize;
        if let Some(c) = edits.as_mut() {
            let out = unsafe { region.edits() };
            while n < MAX_EDITS {
                let Ok(e) = c.pop() else { break };
                out[n] = encode_edit(&e);
                n += 1;
            }
        }
        unsafe { region.header_mut().n_edits = n as u32 };
        h.latency.store(latency, Ordering::Relaxed);
        h.status.store(status, Ordering::Relaxed);
        h.done_seq.store(seq, Ordering::Release);
        done.post();
    }
    edits
}
