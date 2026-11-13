//! One CLAP plugin instance: [`ClapMain`] owns the `PluginInstance`
//! (main thread), [`ClapAudio`] owns the audio processor.

use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use aitrack_dsp::event::Expression;
use aitrack_dsp::{EventKind, ParamInfo, ParamScale, ProcessContext};
use clack_extensions::audio_ports::{AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::gui::{GuiApiType, GuiConfiguration, GuiSize, PluginGui, Window as ClapWindow};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialects, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoBuffer, ParamInfoFlags, PluginParams};
use clack_extensions::state::PluginState;
use clack_extensions::tail::PluginTail;
use clack_extensions::timer::{PluginTimer, TimerId};
use clack_host::events::event_types::*;
use clack_host::events::spaces::CoreEventSpace;
use clack_host::utils::Cookie;
use clack_host::events::{EventFlags, EventHeader};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};

use super::{ClapAudioHandler, ClapFlags, ClapHost, ClapMainThread, ClapShared, host_info, load_entry};
use crate::editor::EditorWindow;
use crate::error::{HostError as Error, Result};
use crate::info::HostedPluginInfo;
use crate::io::BusScratch;
use crate::params::{PARAM_EDIT_QUEUE, ParamEdit, ParamEditKind, PendingParams};
use crate::plugin::{AudioSide, Loaded, MainSide, SharedState, bar_number, bar_start_beats};
use crate::strings::{from_bytes, intern};

const MAX_EVENTS: usize = 2048;

struct GuiState {
    window: Option<EditorWindow>,
    floating: bool,
    size: (u32, u32),
    can_resize: bool,
}

pub(crate) struct ClapMain {
    gui: Option<GuiState>,
    instance: PluginInstance<ClapHost>,
    flags: Arc<ClapFlags>,
    params_ext: Option<PluginParams>,
    state_ext: Option<PluginState>,
    latency_ext: Option<PluginLatency>,
    gui_ext: Option<PluginGui>,
    ports_ext: Option<PluginAudioPorts>,
    timer_ext: Option<PluginTimer>,
    param_ids: Vec<u32>,
}

pub(crate) struct ClapAudio {
    processor: Option<PluginAudioProcessor<ClapHost>>,
    in_events: EventBuffer,
    out_events: EventBuffer,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    in_scratch: BusScratch,
    out_scratch: BusScratch,
    edits: rtrb::Producer<ParamEdit>,
    flags: Arc<ClapFlags>,
    tail_ext: Option<PluginTail>,
    steady: u64,
    /// The note input only speaks MIDI 1.0: send notes as MIDI bytes.
    midi_notes: bool,
    /// Each parameter's cookie from `clap_param_info`, in parameter-list
    /// order, sent back with every value event as the spec asks. JUCE-based
    /// CLAPs (clap-juce-extensions, e.g. Odin 2) read the cookie as their
    /// parameter object and crash on an empty one.
    cookies: Vec<Cookie>,
    /// `(param id, index into cookies)`, sorted by id.
    cookie_index: Vec<(u32, usize)>,
}

// SAFETY: the audio processor is `Send` by clack's design; the extension
// handle is a pointer to a static vtable inside the plugin, valid for the
// instance lifetime and only called from the audio thread.
unsafe impl Send for ClapAudio {}

pub(crate) fn load(info: &HostedPluginInfo) -> Result<Loaded> {
    let entry = load_entry(&info.path)?;
    let mut hosted = super::enumerate(&info.path)?.into_iter().find(|i| i.uid == info.uid).ok_or_else(|| Error::NotFound(info.uid.clone()))?;
    let id = CString::new(info.uid.clone()).map_err(|_| Error::NotFound(info.uid.clone()))?;
    let flags = Arc::new(ClapFlags::default());
    let (f1, f2) = (flags.clone(), flags.clone());
    let mut instance =
        PluginInstance::<ClapHost>::new(move |_| ClapShared { flags: f1 }, move |_| ClapMainThread::new(f2), &entry, &id, &host_info()?)
            .map_err(|e| Error::Instantiate(format!("{e:?}")))?;

    let tail_ext: Option<PluginTail> = instance.plugin_shared_handle().get_extension();
    let handle = instance.plugin_handle();
    let params_ext: Option<PluginParams> = handle.get_extension();
    let state_ext: Option<PluginState> = handle.get_extension();
    let latency_ext: Option<PluginLatency> = handle.get_extension();
    let gui_ext: Option<PluginGui> = handle.get_extension();
    let ports_ext: Option<PluginAudioPorts> = handle.get_extension();
    let note_ext: Option<PluginNotePorts> = handle.get_extension();
    let timer_ext: Option<PluginTimer> = handle.get_extension();

    let mut params = Vec::new();
    let mut values = Vec::new();
    let mut cookies = Vec::new();
    if let Some(pe) = &params_ext {
        let mut buf = ParamInfoBuffer::new();
        for i in 0..pe.count(&handle) {
            let Some(p) = pe.get_info(&handle, i, &mut buf) else { continue };
            let id = p.id.get();
            let name = from_bytes(p.name);
            let (min, max) = (p.min_value as f32, p.max_value as f32);
            let mut info = ParamInfo::new(id, intern(&name), min, max, p.default_value as f32);
            if p.flags.contains(ParamInfoFlags::IS_STEPPED) {
                info.scale = ParamScale::Stepped;
            }
            // Hidden and read-only parameters are not offered for automation.
            info.automatable = p.flags.contains(ParamInfoFlags::IS_AUTOMATABLE) && !p.flags.intersects(ParamInfoFlags::IS_READONLY | ParamInfoFlags::IS_HIDDEN);
            let plain = pe.get_value(&handle, p.id).unwrap_or(p.default_value);
            let range = (p.max_value - p.min_value).abs();
            let n = if range < f64::EPSILON { 0.0 } else { ((plain - p.min_value) / (p.max_value - p.min_value)).clamp(0.0, 1.0) };
            values.push((id, n as f32));
            cookies.push(p.cookie);
            params.push(info);
        }
    }

    let (in_ch, out_ch) = port_channels(ports_ext.as_ref(), &handle);
    hosted.inputs = in_ch.first().copied().unwrap_or(0) as u16;
    hosted.outputs = out_ch.first().copied().unwrap_or(0) as u16;
    hosted.params = params.len() as u32;
    hosted.probed = true;
    if let Some(np) = &note_ext
        && np.count(&handle, true) > 0
        && in_ch.is_empty()
    {
        hosted.is_instrument = true;
    }
    let midi_notes = note_ext.as_ref().is_some_and(|np| {
        let mut buf = NotePortInfoBuffer::new();
        np.count(&handle, true) > 0 && np.get(&handle, 0, true, &mut buf).is_some_and(|p| midi_only(p.supported_dialects))
    });

    let (edits_tx, edits_rx) = rtrb::RingBuffer::new(PARAM_EDIT_QUEUE);
    let main = ClapMain {
        gui: None,
        instance,
        flags: flags.clone(),
        params_ext,
        state_ext,
        latency_ext,
        gui_ext,
        ports_ext,
        timer_ext,
        param_ids: params.iter().map(|p| p.id).collect(),
    };
    let audio = ClapAudio {
        processor: None,
        in_events: EventBuffer::with_capacity(MAX_EVENTS),
        out_events: EventBuffer::with_capacity(MAX_EVENTS),
        in_ports: AudioPorts::with_capacity(0, 0),
        out_ports: AudioPorts::with_capacity(0, 0),
        in_scratch: BusScratch::new(&[], 0),
        out_scratch: BusScratch::new(&[], 0),
        edits: edits_tx,
        flags,
        tail_ext,
        steady: 0,
        midi_notes,
        cookie_index: {
            let mut ix: Vec<(u32, usize)> = params.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
            ix.sort_unstable();
            ix
        },
        cookies,
    };
    Ok(Loaded { main: MainSide::Clap(Box::new(main)), audio: AudioSide::Clap(Box::new(audio)), info: hosted, params, values, edits: edits_rx })
}

fn port_channels(ports: Option<&PluginAudioPorts>, handle: &PluginMainThreadHandle) -> (Vec<usize>, Vec<usize>) {
    let Some(ports) = ports else { return (Vec::new(), vec![2]) };
    let mut buf = AudioPortInfoBuffer::new();
    let mut read = |is_input: bool| -> Vec<usize> {
        (0..ports.count(handle, is_input)).map(|i| ports.get(handle, i, is_input, &mut buf).map_or(0, |p| p.channel_count as usize)).collect()
    };
    (read(true), read(false))
}

impl ClapMain {
    pub fn prepare(&mut self, audio: &mut ClapAudio, state: &SharedState, sample_rate: f64, max_frames: usize) -> Result<()> {
        self.deactivate(audio);
        self.flags.restart.store(false, Ordering::Release);
        let max_frames = max_frames.max(1);
        let (in_ch, out_ch) = {
            let handle = self.instance.plugin_handle();
            port_channels(self.ports_ext.as_ref(), &handle)
        };
        audio.configure(&in_ch, &out_ch, max_frames);
        let flags = self.flags.clone();
        let config = PluginAudioConfiguration { sample_rate, min_frames_count: 1, max_frames_count: max_frames as u32 };
        let stopped = self.instance.activate(move |_, _| ClapAudioHandler { flags }, config).map_err(|e| Error::Activate(format!("{e:?}")))?;
        audio.processor = Some(stopped.into());
        audio.flags.tail_changed.store(true, Ordering::Release);
        let latency = match &self.latency_ext {
            Some(l) => l.get(&self.instance.plugin_handle()),
            None => 0,
        };
        state.latency.store(latency, Ordering::Relaxed);
        self.flags.latency_changed.store(false, Ordering::Release);
        Ok(())
    }

    pub fn deactivate(&mut self, audio: &mut ClapAudio) {
        if let Some(p) = audio.processor.take() {
            self.instance.deactivate(p.into_stopped());
        }
    }

    pub fn save_state(&mut self) -> Result<Vec<u8>> {
        let ext = self.state_ext.ok_or_else(|| Error::State("plugin has no state extension".into()))?;
        let mut out = Vec::new();
        ext.save(&self.instance.plugin_handle(), &mut out).map_err(|e| Error::State(format!("{e:?}")))?;
        Ok(out)
    }

    pub fn load_state(&mut self, state: &SharedState, data: &[u8]) -> Result<()> {
        let ext = self.state_ext.ok_or_else(|| Error::State("plugin has no state extension".into()))?;
        let mut reader = data;
        ext.load(&self.instance.plugin_handle(), &mut reader).map_err(|e| Error::State(format!("{e:?}")))?;
        self.refresh_values(state);
        Ok(())
    }

    fn refresh_values(&mut self, state: &SharedState) {
        let Some(pe) = self.params_ext else { return };
        let handle = self.instance.plugin_handle();
        for &id in &self.param_ids {
            if let Some(v) = pe.get_value(&handle, ClapId::new(id)) {
                state.params.set(id, state.params.to_normalized(id, v));
            }
        }
    }

    pub fn param_text(&mut self, state: &SharedState, id: u32, normalized: f32) -> Option<String> {
        let pe = self.params_ext?;
        let plain = state.params.to_plain(id, normalized);
        let mut buf = [0u8; 256];
        let handle = self.instance.plugin_handle();
        pe.value_to_text(&handle, ClapId::new(id), plain, &mut buf).ok().map(|b| from_bytes(b))
    }

    pub fn has_editor(&mut self) -> bool {
        self.gui_ext.is_some()
    }

    pub fn is_editor_open(&self) -> bool {
        self.gui.is_some()
    }

    pub fn raise_editor(&mut self) {
        let Some(state) = &self.gui else { return };
        if let Some(window) = &state.window {
            window.raise();
        } else if state.floating
            && let Some(gui) = self.gui_ext
        {
            let handle = self.instance.plugin_handle();
            let _ = gui.show(&handle);
        }
    }

    fn api() -> GuiApiType<'static> {
        #[cfg(target_os = "macos")]
        {
            GuiApiType::COCOA
        }
        #[cfg(not(target_os = "macos"))]
        {
            GuiApiType::WIN32
        }
    }

    pub fn open_editor(&mut self, title: &str) -> Result<()> {
        let gui = self.gui_ext.ok_or(Error::NoEditor)?;
        let handle = self.instance.plugin_handle();
        let embedded = GuiConfiguration { api_type: Self::api(), is_floating: false };
        if gui.is_api_supported(&handle, embedded) {
            gui.create(&handle, embedded).map_err(|e| Error::Editor(format!("gui.create: {e:?}")))?;
            let can_resize = gui.can_resize(&handle);
            let window = match EditorWindow::new(title, 400, 300, can_resize) {
                Ok(w) => w,
                Err(e) => {
                    gui.destroy(&handle);
                    return Err(e);
                }
            };
            if !cfg!(target_os = "macos") {
                let _ = gui.set_scale(&handle, window.scale_factor());
            }
            let size = gui.get_size(&handle).map_or((400, 300), |s| (s.width.max(1), s.height.max(1)));
            window.set_client_size(size.0, size.1);
            #[cfg(target_os = "macos")]
            let parent = unsafe { ClapWindow::from_cocoa_nsview(window.parent_ptr()) };
            #[cfg(not(target_os = "macos"))]
            let parent = unsafe { ClapWindow::from_win32_hwnd(window.parent_ptr()) };
            if let Err(e) = unsafe { gui.set_parent(&handle, parent) } {
                gui.destroy(&handle);
                return Err(Error::Editor(format!("gui.set_parent: {e:?}")));
            }
            let _ = gui.show(&handle);
            window.show();
            self.gui = Some(GuiState { window: Some(window), floating: false, size, can_resize });
            return Ok(());
        }
        let floating = GuiConfiguration { api_type: Self::api(), is_floating: true };
        if gui.is_api_supported(&handle, floating) {
            gui.create(&handle, floating).map_err(|e| Error::Editor(format!("gui.create(floating): {e:?}")))?;
            if let Ok(t) = CString::new(title) {
                gui.suggest_title(&handle, &t);
            }
            let _ = gui.show(&handle);
            self.flags.gui_closed.store(false, Ordering::Release);
            self.gui = Some(GuiState { window: None, floating: true, size: (0, 0), can_resize: false });
            return Ok(());
        }
        Err(Error::Editor("plugin supports neither embedded nor floating GUIs on this platform".into()))
    }

    pub fn close_editor(&mut self) {
        let Some(state) = self.gui.take() else { return };
        if let Some(gui) = self.gui_ext {
            let handle = self.instance.plugin_handle();
            let _ = gui.hide(&handle);
            gui.destroy(&handle);
        }
        drop(state.window);
    }

    pub fn idle(&mut self, state: &SharedState) {
        if self.flags.callback.swap(false, Ordering::AcqRel) {
            self.instance.call_on_main_thread_callback();
        }
        if self.flags.restart.load(Ordering::Acquire) {
            state.needs_restart.store(true, Ordering::Release);
        }
        if self.flags.latency_changed.swap(false, Ordering::AcqRel) {
            // CLAP: a latency change takes effect on re-activation.
            state.latency_changed.store(true, Ordering::Release);
            state.needs_restart.store(true, Ordering::Release);
        }
        if self.flags.state_dirty.swap(false, Ordering::AcqRel) {
            state.state_dirty.store(true, Ordering::Release);
        }
        if self.flags.params_rescan.swap(false, Ordering::AcqRel) {
            self.refresh_values(state);
        }
        self.fire_timers();
        self.poll_gui();
    }

    fn fire_timers(&mut self) {
        let Some(timer) = self.timer_ext else { return };
        let now = Instant::now();
        let due: Vec<u32> = self.instance.access_handler(|h: &ClapMainThread| {
            let mut timers = h.timers.borrow_mut();
            timers
                .iter_mut()
                .filter(|t| t.next <= now)
                .map(|t| {
                    t.next = now + t.period;
                    t.id
                })
                .collect()
        });
        if due.is_empty() {
            return;
        }
        let handle = self.instance.plugin_handle();
        for id in due {
            timer.on_timer(&handle, TimerId(id));
        }
    }

    fn poll_gui(&mut self) {
        let Some(gui) = self.gui_ext else { return };
        let Some(gs) = self.gui.as_mut() else { return };
        if gs.floating {
            if self.flags.gui_closed.swap(false, Ordering::AcqRel) {
                self.close_editor();
            }
            return;
        }
        if gs.window.as_ref().is_some_and(|w| w.user_closed()) {
            self.close_editor();
            return;
        }
        let Some(window) = gs.window.as_ref() else { return };
        let handle = self.instance.plugin_handle();
        if self.flags.resize.swap(false, Ordering::AcqRel) {
            let packed = self.flags.resize_size.load(Ordering::Acquire);
            let size = ((packed >> 32) as u32, packed as u32);
            gs.size = size;
            window.set_client_size(size.0, size.1);
            return;
        }
        let client = window.client_size();
        if client.0 == 0 || client.1 == 0 || client == gs.size {
            return;
        }
        if gs.can_resize {
            let want = GuiSize { width: client.0, height: client.1 };
            let adj = gui.adjust_size(&handle, want).unwrap_or(want);
            if gui.set_size(&handle, adj).is_ok() {
                gs.size = (adj.width, adj.height);
                if gs.size != client {
                    window.set_client_size(adj.width, adj.height);
                }
                return;
            }
        }
        window.set_client_size(gs.size.0, gs.size.1);
    }
}

impl Drop for ClapMain {
    fn drop(&mut self) {
        self.close_editor();
    }
}

impl ClapAudio {
    fn configure(&mut self, in_ch: &[usize], out_ch: &[usize], max_frames: usize) {
        self.in_scratch = BusScratch::new(in_ch, max_frames);
        self.out_scratch = BusScratch::new(out_ch, max_frames);
        self.in_ports = AudioPorts::with_capacity(in_ch.iter().sum(), in_ch.len());
        self.out_ports = AudioPorts::with_capacity(out_ch.iter().sum(), out_ch.len());
        self.steady = 0;
    }

    pub fn reset(&mut self) {
        if let Some(p) = self.processor.as_mut()
            && let Ok(s) = p.as_started_mut()
        {
            s.reset();
        }
    }

    #[inline]
    fn push<E: AsRef<UnknownEvent>>(events: &mut EventBuffer, count: &mut usize, e: &E) {
        if *count < MAX_EVENTS {
            events.push(e);
            *count += 1;
        }
    }

    pub fn process(&mut self, ctx: &mut ProcessContext<'_>, state: &SharedState, pending: &mut PendingParams) -> bool {
        let Some(processor) = self.processor.as_mut() else { return false };
        let frames = ctx.buffer.frames().min(self.in_scratch.max_frames().max(self.out_scratch.max_frames()));
        if frames == 0 {
            return true;
        }
        let Ok(started) = processor.ensure_processing_started() else { return false };

        // ── Events (sorted: pending set_param values first, at 0) ──
        self.in_events.clear();
        self.out_events.clear();
        let mut n = 0usize;
        let infos = state.params.infos();
        let events = &mut self.in_events;
        let (cookies, cookie_index) = (&self.cookies, &self.cookie_index);
        let cookie_of = |id: u32| cookie_index.binary_search_by_key(&id, |e| e.0).ok().and_then(|i| cookies.get(cookie_index[i].1)).copied().unwrap_or_default();
        pending.drain(|index, v| {
            let id = infos[index].id;
            // SAFETY: the cookie is the one this parameter reported in its info.
            let ev = unsafe { ParamValueEvent::new(0, ClapId::new(id), Pckn::match_all(), state.params.to_plain(id, v)).with_cookie(cookie_of(id)) };
            Self::push(events, &mut n, &ev);
        });
        for e in ctx.events {
            let t = e.offset.min(frames as u32 - 1);
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, .. } if self.midi_notes => {
                    let ev = MidiEvent::new(t, 0, [0x90 | (channel & 15), key & 127, midi_velocity(velocity).max(1)]);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::NoteOff { channel, key, velocity, .. } if self.midi_notes => {
                    let ev = MidiEvent::new(t, 0, [0x80 | (channel & 15), key & 127, midi_velocity(velocity)]);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::NoteOn { channel, key, velocity, note_id } => {
                    let ev = NoteOnEvent::new(t, pckn(channel, key, note_id), velocity as f64);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::NoteOff { channel, key, velocity, note_id } => {
                    let ev = NoteOffEvent::new(t, pckn(channel, key, note_id), velocity as f64);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::NoteExpression { note_id, key, expr, value } => {
                    let (kind, v) = match expr {
                        Expression::Tuning => (NoteExpressionType::Tuning, value as f64),
                        Expression::Pressure => (NoteExpressionType::Pressure, value as f64),
                        Expression::Pan => (NoteExpressionType::Pan, (value as f64 + 1.0) * 0.5),
                        Expression::Brightness => (NoteExpressionType::Brightness, value as f64),
                        Expression::Volume => (NoteExpressionType::Volume, value as f64),
                    };
                    let ev = NoteExpressionEvent::new(t, pckn(0, key, note_id), kind, v);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::Param { id, value } => {
                    state.params.set(id, value);
                    // SAFETY: as above.
                    let ev = unsafe { ParamValueEvent::new(t, ClapId::new(id), Pckn::match_all(), state.params.to_plain(id, value)).with_cookie(cookie_of(id)) };
                    Self::push(events, &mut n, &ev);
                }
                EventKind::ControlChange { channel, cc, value } => {
                    let ev = MidiEvent::new(t, 0, [0xB0 | (channel & 15), cc & 127, (value.clamp(0.0, 1.0) * 127.0).round() as u8]);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::PitchBend { channel, value } => {
                    let v = (((value.clamp(-1.0, 1.0) + 1.0) * 0.5) * 16383.0).round() as u16;
                    let ev = MidiEvent::new(t, 0, [0xE0 | (channel & 15), (v & 127) as u8, (v >> 7) as u8]);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::ChannelPressure { channel, value } => {
                    let ev = MidiEvent::new(t, 0, [0xD0 | (channel & 15), (value.clamp(0.0, 1.0) * 127.0).round() as u8, 0]);
                    Self::push(events, &mut n, &ev);
                }
                EventKind::AllNotesOff => {
                    for ch in 0..16u8 {
                        let ev = MidiEvent::new(t, 0, [0xB0 | ch, 123, 0]);
                        Self::push(events, &mut n, &ev);
                    }
                }
            }
        }

        // ── Audio ──
        for bus in 0..self.in_scratch.bus_count() {
            match bus {
                0 => self.in_scratch.fill_from(0, Some(ctx.buffer), frames),
                1 => self.in_scratch.fill_from(1, ctx.sidechain, frames),
                _ => self.in_scratch.clear(bus, frames),
            }
        }
        for bus in 0..self.out_scratch.bus_count() {
            self.out_scratch.clear(bus, frames);
        }
        let transport = make_transport(ctx);
        let result = {
            let input = self.in_ports.with_input_buffers(self.in_scratch.buses_mut().iter_mut().map(|chs| AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(chs.iter_mut().map(|c| InputChannel::variable(&mut c[..frames]))),
            }));
            let mut output =
                self.out_ports.with_output_buffers(self.out_scratch.buses_mut().iter_mut().map(|chs| AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(chs.iter_mut().map(|c| &mut c[..frames])),
                }));
            started.process(&input, &mut output, &self.in_events.as_input(), &mut self.out_events.as_output(), Some(self.steady), Some(&transport))
        };
        self.steady += frames as u64;
        self.out_scratch.write_to(0, ctx.buffer);

        // ── Output events: GUI edits travel back as param events ──
        for e in self.out_events.iter() {
            let edit = match e.as_core_event() {
                Some(CoreEventSpace::ParamValue(ev)) => ev.param_id().map(|id| {
                    let n = state.params.to_normalized(id.get(), ev.value());
                    state.params.set(id.get(), n);
                    ParamEdit { id: id.get(), value: n, kind: ParamEditKind::Value }
                }),
                Some(CoreEventSpace::ParamGestureBegin(ev)) => {
                    ev.param_id().map(|id| ParamEdit { id: id.get(), value: f32::NAN, kind: ParamEditKind::Begin })
                }
                Some(CoreEventSpace::ParamGestureEnd(ev)) => {
                    ev.param_id().map(|id| ParamEdit { id: id.get(), value: f32::NAN, kind: ParamEditKind::End })
                }
                _ => None,
            };
            if let Some(edit) = edit {
                let _ = self.edits.push(edit);
            }
        }

        if self.flags.tail_changed.swap(false, Ordering::AcqRel)
            && let Some(tail) = self.tail_ext
        {
            let t = tail.get(&started.plugin_handle());
            let samples = if t.is_infinite() { u32::MAX } else { t.to_raw() };
            state.tail.store(samples, Ordering::Relaxed);
        }
        result.is_ok()
    }
}

/// A note port that accepts MIDI 1.0 but not CLAP note events.
fn midi_only(dialects: NoteDialects) -> bool {
    dialects.contains(NoteDialects::MIDI) && !dialects.contains(NoteDialects::CLAP)
}

fn midi_velocity(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 127.0).round() as u8
}

fn pckn(channel: u8, key: u8, note_id: i32) -> Pckn {
    if note_id >= 0 {
        Pckn::new(0u16, channel as u16, key as u16, note_id as u32)
    } else {
        Pckn::new(0u16, channel as u16, key as u16, clack_host::events::Match::All)
    }
}

fn make_transport(ctx: &ProcessContext<'_>) -> TransportEvent {
    let t = ctx.transport;
    let mut flags =
        TransportFlags::HAS_TEMPO | TransportFlags::HAS_BEATS_TIMELINE | TransportFlags::HAS_SECONDS_TIMELINE | TransportFlags::HAS_TIME_SIGNATURE;
    if t.playing {
        flags |= TransportFlags::IS_PLAYING;
    }
    if t.recording {
        flags |= TransportFlags::IS_RECORDING;
    }
    let spb = 60.0 / t.bpm.max(1.0);
    let (ls, le) = t.loop_range_beats.unwrap_or((0.0, 0.0));
    if t.loop_range_beats.is_some() {
        flags |= TransportFlags::IS_LOOP_ACTIVE;
    }
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(t.position_beats),
        song_pos_seconds: SecondsTime::from_float(t.position_samples as f64 / ctx.sample_rate.max(1.0)),
        tempo: t.bpm,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_float(ls),
        loop_end_beats: BeatTime::from_float(le),
        loop_start_seconds: SecondsTime::from_float(ls * spb),
        loop_end_seconds: SecondsTime::from_float(le * spb),
        bar_start: BeatTime::from_float(bar_start_beats(t)),
        bar_number: bar_number(t),
        time_signature_numerator: t.time_sig.0 as u16,
        time_signature_denominator: t.time_sig.1 as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_only_note_ports_get_midi_notes() {
        assert!(midi_only(NoteDialects::MIDI));
        assert!(!midi_only(NoteDialects::CLAP | NoteDialects::MIDI));
        assert!(!midi_only(NoteDialects::CLAP));
        assert!(!midi_only(NoteDialects::empty()));
        assert_eq!(midi_velocity(0.8), 102);
        assert_eq!(midi_velocity(2.0), 127);
        assert_eq!(midi_velocity(0.0), 0);
    }
}
