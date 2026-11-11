//! One VST3 plugin instance, split into the main-thread half
//! ([`Vst3Main`]: component setup, controller, state, editor) and the audio
//! half ([`Vst3Audio`]: `IAudioProcessor::process`).

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use aitrack_dsp::event::Expression;
use aitrack_dsp::{EventKind, ParamInfo, ParamScale, ProcessContext};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{ComPtr, ComWrapper, Interface};

use super::com::*;
use super::module::Vst3Module;
use super::{entry_to_info, iid_ptr};
use crate::editor::EditorWindow;
use crate::error::{HostError, Result};
use crate::info::{HostedPluginInfo, hex_to_tuid};
use crate::io::BusScratch;
use crate::params::{PARAM_EDIT_QUEUE, PendingParams};
use crate::plugin::{AudioSide, Loaded, MainSide, SharedState, bar_start_beats};
use crate::strings::{from_utf16, intern};

const OK: tresult = vst3::Steinberg::kResultOk;
const STATE_MAGIC: &[u8; 4] = b"AIV3";
/// Channels × (CC 0..=127, aftertouch 128, pitch bend 129).
const MIDI_MAP_STRIDE: usize = 130;
const NO_PARAM: u32 = u32::MAX;

fn audio() -> MediaType {
    MediaTypes_::kAudio as MediaType
}
fn event() -> MediaType {
    MediaTypes_::kEvent as MediaType
}
fn input() -> BusDirection {
    BusDirections_::kInput as BusDirection
}
fn output() -> BusDirection {
    BusDirections_::kOutput as BusDirection
}

unsafe fn create<I: Interface>(factory: &ComPtr<IPluginFactory>, cid: &TUID) -> Option<ComPtr<I>> {
    let mut obj: *mut c_void = std::ptr::null_mut();
    let r = unsafe { factory.createInstance(cid.as_ptr(), iid_ptr::<I>(), &mut obj) };
    if r == OK { unsafe { ComPtr::from_raw(obj as *mut I) } } else { None }
}

pub(crate) struct Vst3Main {
    view: Option<ComPtr<IPlugView>>,
    window: Option<EditorWindow>,
    connections: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    controller: Option<ComPtr<IEditController>>,
    separate_controller: bool,
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    handler: ComWrapper<ComponentHandler>,
    frame: ComWrapper<PlugFrame>,
    host_app: ComWrapper<HostApp>,
    to_controller: rtrb::Consumer<(u32, f64)>,
    param_ids: Vec<u32>,
    active: bool,
    // Last: the module must outlive every COM pointer above.
    _module: Arc<Vst3Module>,
}

pub(crate) struct Vst3Audio {
    processor: ComPtr<IAudioProcessor>,
    in_scratch: BusScratch,
    out_scratch: BusScratch,
    in_bufs: Vec<AudioBusBuffers>,
    out_bufs: Vec<AudioBusBuffers>,
    in_changes: ComWrapper<ParamChanges>,
    out_changes: ComWrapper<ParamChanges>,
    in_events: ComWrapper<EventList>,
    out_events: ComWrapper<EventList>,
    context: Box<SdkProcessContext>,
    from_gui: rtrb::Consumer<(u32, f64)>,
    to_controller: rtrb::Producer<(u32, f64)>,
    midi_map: Box<[u32]>,
    has_event_input: bool,
    processing: bool,
    sample_rate: f64,
    _module: Arc<Vst3Module>,
}

/// The SDK's `ProcessContext` struct (the name clashes with ours).
type SdkProcessContext = vst3::Steinberg::Vst::ProcessContext;

// SAFETY: `AudioBusBuffers` holds raw pointers into `in_scratch` /
// `out_scratch`, owned by the same struct. The COM pointers are to objects
// the VST3 spec lets the host call from the audio thread.
unsafe impl Send for Vst3Audio {}

fn host_unknown(app: &ComWrapper<HostApp>) -> *mut FUnknown {
    app.as_com_ref::<IHostApplication>().map_or(std::ptr::null_mut(), |r| r.as_ptr() as *mut FUnknown)
}

/// Instantiate `info` (a processor class id in `info.path`).
pub(crate) fn load(info: &HostedPluginInfo) -> Result<Loaded> {
    let module = Vst3Module::load(&info.path)?;
    let cid_bytes = hex_to_tuid(&info.uid).ok_or_else(|| HostError::NotFound(info.uid.clone()))?;
    let cid: TUID = cid_bytes.map(|b| b as i8);
    let factory = module.factory();
    let entry = super::audio_classes(factory).into_iter().find(|e| e.cid == cid);
    let mut hosted = match &entry {
        Some(e) => entry_to_info(&info.path, e),
        None => return Err(HostError::NotFound(info.uid.clone())),
    };

    let host_app = ComWrapper::new(HostApp);
    let host = host_unknown(&host_app);
    unsafe {
        if let Some(f3) = factory.cast::<IPluginFactory3>() {
            f3.setHostContext(host);
        }
    }

    let component: ComPtr<IComponent> =
        unsafe { create(factory, &cid) }.ok_or_else(|| HostError::Instantiate("createInstance(IComponent) failed".into()))?;
    if unsafe { component.initialize(host) } != OK {
        return Err(HostError::Instantiate("IComponent::initialize failed".into()));
    }
    let Some(processor) = component.cast::<IAudioProcessor>() else {
        unsafe { component.terminate() };
        return Err(HostError::Instantiate("component has no IAudioProcessor".into()));
    };

    // Single-component plugins implement IEditController on the component;
    // split plugins name a separate controller class.
    let (controller, separate_controller) = match component.cast::<IEditController>() {
        Some(c) => (Some(c), false),
        None => {
            let mut ccid: TUID = [0; 16];
            if unsafe { component.getControllerClassId(&mut ccid) } == OK {
                match unsafe { create::<IEditController>(factory, &ccid) } {
                    Some(c) if unsafe { c.initialize(host) } == OK => (Some(c), true),
                    _ => {
                        log::warn!("{}: controller class could not be created", hosted.name);
                        (None, false)
                    }
                }
            } else {
                (None, false)
            }
        }
    };

    let connections = match (&controller, separate_controller) {
        (Some(c), true) => match (component.cast::<IConnectionPoint>(), c.cast::<IConnectionPoint>()) {
            (Some(a), Some(b)) => unsafe {
                a.connect(b.as_ptr());
                b.connect(a.as_ptr());
                Some((a, b))
            },
            _ => None,
        },
        _ => None,
    };

    let (edits_tx, edits_rx) = rtrb::RingBuffer::new(PARAM_EDIT_QUEUE);
    let (gui_tx, gui_rx) = rtrb::RingBuffer::new(1024);
    let (ctl_tx, ctl_rx) = rtrb::RingBuffer::new(4096);
    let handler = ComWrapper::new(ComponentHandler::new(edits_tx, gui_tx));

    let mut params = Vec::new();
    let mut values = Vec::new();
    let mut midi_map = vec![NO_PARAM; 16 * MIDI_MAP_STRIDE].into_boxed_slice();
    if let Some(c) = &controller {
        unsafe {
            if let Some(h) = handler.as_com_ref::<IComponentHandler>() {
                c.setComponentHandler(h.as_ptr());
            }
            // The controller starts from the component's current state.
            let stream = ComWrapper::new(MemoryStream::default());
            let sp = stream.as_com_ref::<IBStream>().map_or(std::ptr::null_mut(), |r| r.as_ptr());
            if component.getState(sp) == OK {
                stream.rewind();
                c.setComponentState(sp);
            }
        }
        read_params(c, &mut params, &mut values);
        if let Some(mm) = c.cast::<IMidiMapping>() {
            for ch in 0..16i16 {
                for cc in 0..MIDI_MAP_STRIDE as i16 {
                    let mut id: ParamID = 0;
                    if unsafe { mm.getMidiControllerAssignment(0, ch, cc, &mut id) } == OK {
                        midi_map[ch as usize * MIDI_MAP_STRIDE + cc as usize] = id;
                    }
                }
            }
        }
    }

    unsafe {
        hosted.inputs = main_bus_channels(&component, input());
        hosted.outputs = main_bus_channels(&component, output());
    }
    hosted.params = params.len() as u32;
    hosted.probed = true;
    let has_event_input = unsafe { component.getBusCount(event(), input()) } > 0;

    let main = Vst3Main {
        view: None,
        window: None,
        connections,
        controller,
        separate_controller,
        processor: processor.clone(),
        component,
        handler,
        frame: ComWrapper::new(PlugFrame::new()),
        host_app,
        to_controller: ctl_rx,
        param_ids: params.iter().map(|p| p.id).collect(),
        active: false,
        _module: module.clone(),
    };
    let audio = Vst3Audio {
        processor,
        in_scratch: BusScratch::new(&[], 0),
        out_scratch: BusScratch::new(&[], 0),
        in_bufs: Vec::new(),
        out_bufs: Vec::new(),
        in_changes: ComWrapper::new(ParamChanges::new()),
        out_changes: ComWrapper::new(ParamChanges::new()),
        in_events: ComWrapper::new(EventList::new()),
        out_events: ComWrapper::new(EventList::new()),
        context: Box::new(unsafe { std::mem::zeroed() }),
        from_gui: gui_rx,
        to_controller: ctl_tx,
        midi_map,
        has_event_input,
        processing: false,
        sample_rate: 48_000.0,
        _module: module,
    };
    Ok(Loaded {
        main: MainSide::Vst3(Box::new(main)),
        audio: AudioSide::Vst3(Box::new(audio)),
        info: hosted,
        params,
        values,
        edits: edits_rx,
    })
}

/// Instantiate briefly to read `(inputs, outputs, params)`.
pub(crate) fn probe(info: &HostedPluginInfo) -> Result<(u16, u16, u32)> {
    let loaded = load(info)?;
    Ok((loaded.info.inputs, loaded.info.outputs, loaded.info.params))
}

unsafe fn main_bus_channels(component: &ComPtr<IComponent>, dir: BusDirection) -> u16 {
    unsafe {
        if component.getBusCount(audio(), dir) == 0 {
            return 0;
        }
        let mut bi: BusInfo = std::mem::zeroed();
        if component.getBusInfo(audio(), dir, 0, &mut bi) == OK { bi.channelCount.max(0) as u16 } else { 2 }
    }
}

fn read_params(c: &ComPtr<IEditController>, params: &mut Vec<ParamInfo>, values: &mut Vec<(u32, f32)>) {
    let n = unsafe { c.getParameterCount() };
    for i in 0..n {
        let mut pi: ParameterInfo = unsafe { std::mem::zeroed() };
        if unsafe { c.getParameterInfo(i, &mut pi) } != OK {
            continue;
        }
        let steps = pi.stepCount.max(0);
        let max = if steps > 0 { steps as f32 } else { 1.0 };
        let title = from_utf16(&pi.title);
        let short = from_utf16(&pi.shortTitle);
        let mut p = ParamInfo::new(pi.id, intern(&title), 0.0, max, pi.defaultNormalizedValue as f32 * max);
        p.short = if short.is_empty() { p.name } else { intern(&short) };
        p.unit = intern(&from_utf16(&pi.units));
        if steps > 0 {
            p.scale = ParamScale::Stepped;
        }
        let flags = pi.flags;
        p.automatable = flags & ParameterInfo_::ParameterFlags_::kCanAutomate as i32 != 0
            && flags & ParameterInfo_::ParameterFlags_::kIsReadOnly as i32 == 0;
        values.push((pi.id, unsafe { c.getParamNormalized(pi.id) } as f32));
        params.push(p);
    }
}

impl Vst3Main {
    fn stream_ptr(s: &ComWrapper<MemoryStream>) -> *mut IBStream {
        s.as_com_ref::<IBStream>().map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }

    pub fn prepare(&mut self, audio: &mut Vst3Audio, state: &SharedState, sample_rate: f64, max_frames: usize) -> Result<()> {
        self.deactivate(audio);
        let max_frames = max_frames.max(1);
        unsafe {
            let n_in = self.component.getBusCount(audio_t(), input());
            let n_out = self.component.getBusCount(audio_t(), output());
            let mut in_arr: Vec<SpeakerArrangement> = (0..n_in).map(|i| self.arrangement(input(), i)).collect();
            let mut out_arr: Vec<SpeakerArrangement> = (0..n_out).map(|i| self.arrangement(output(), i)).collect();
            // Main buses stereo; auxiliary buses keep their own layout.
            if let Some(a) = in_arr.first_mut() {
                *a = SpeakerArr::kStereo;
            }
            if let Some(a) = out_arr.first_mut() {
                *a = SpeakerArr::kStereo;
            }
            if self.processor.setBusArrangements(in_arr.as_mut_ptr(), n_in, out_arr.as_mut_ptr(), n_out) != OK {
                log::info!("{}: stereo bus arrangement refused, using the plugin's defaults", state.hosted.name);
            }
            let in_ch: Vec<usize> = (0..n_in).map(|i| self.arrangement(input(), i).count_ones() as usize).collect();
            let out_ch: Vec<usize> = (0..n_out).map(|i| self.arrangement(output(), i).count_ones() as usize).collect();
            for i in 0..n_in {
                self.component.activateBus(audio_t(), input(), i, 1);
            }
            for i in 0..n_out {
                self.component.activateBus(audio_t(), output(), i, 1);
            }
            if self.component.getBusCount(event(), input()) > 0 {
                self.component.activateBus(event(), input(), 0, 1);
            }
            let mut setup = ProcessSetup {
                processMode: ProcessModes_::kRealtime as i32,
                symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
                maxSamplesPerBlock: max_frames as i32,
                sampleRate: sample_rate,
            };
            if self.processor.setupProcessing(&mut setup) != OK {
                return Err(HostError::Activate("setupProcessing failed".into()));
            }
            if self.component.setActive(1) != OK {
                return Err(HostError::Activate("setActive(true) failed".into()));
            }
            self.active = true;
            audio.configure(&in_ch, &out_ch, max_frames, sample_rate);
            state.latency.store(self.processor.getLatencySamples(), Ordering::Relaxed);
            state.tail.store(self.processor.getTailSamples(), Ordering::Relaxed);
        }
        Ok(())
    }

    unsafe fn arrangement(&self, dir: BusDirection, index: i32) -> SpeakerArrangement {
        let mut arr: SpeakerArrangement = 0;
        if unsafe { self.processor.getBusArrangement(dir, index, &mut arr) } == OK && arr != 0 {
            return arr;
        }
        let mut bi: BusInfo = unsafe { std::mem::zeroed() };
        if unsafe { self.component.getBusInfo(audio_t(), dir, index, &mut bi) } == OK {
            return match bi.channelCount {
                1 => SpeakerArr::kMono,
                n if n > 0 => (1u64 << n.min(64)) - 1,
                _ => 0,
            };
        }
        SpeakerArr::kStereo
    }

    pub fn deactivate(&mut self, audio: &mut Vst3Audio) {
        unsafe {
            if audio.processing {
                self.processor.setProcessing(0);
                audio.processing = false;
            }
            if self.active {
                self.component.setActive(0);
                self.active = false;
            }
        }
    }

    pub fn save_state(&mut self) -> Result<Vec<u8>> {
        let comp = ComWrapper::new(MemoryStream::default());
        if unsafe { self.component.getState(Self::stream_ptr(&comp)) } != OK {
            return Err(HostError::State("IComponent::getState failed".into()));
        }
        let ctrl = ComWrapper::new(MemoryStream::default());
        if let Some(c) = &self.controller {
            unsafe { c.getState(Self::stream_ptr(&ctrl)) };
        }
        let (a, b) = (comp.bytes(), ctrl.bytes());
        let mut out = Vec::with_capacity(16 + a.len() + b.len());
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(a.len() as u32).to_le_bytes());
        out.extend_from_slice(&a);
        out.extend_from_slice(&(b.len() as u32).to_le_bytes());
        out.extend_from_slice(&b);
        Ok(out)
    }

    pub fn load_state(&mut self, state: &SharedState, data: &[u8]) -> Result<()> {
        let (comp, ctrl) = split_state(data).ok_or_else(|| HostError::State("corrupt VST3 state".into()))?;
        let cs = ComWrapper::new(MemoryStream::from_bytes(comp));
        if unsafe { self.component.setState(Self::stream_ptr(&cs)) } != OK {
            return Err(HostError::State("IComponent::setState failed".into()));
        }
        if let Some(c) = &self.controller {
            cs.rewind();
            unsafe { c.setComponentState(Self::stream_ptr(&cs)) };
            if !ctrl.is_empty() {
                let ks = ComWrapper::new(MemoryStream::from_bytes(ctrl));
                unsafe { c.setState(Self::stream_ptr(&ks)) };
            }
            for &id in &self.param_ids {
                state.params.set(id, unsafe { c.getParamNormalized(id) } as f32);
            }
        }
        Ok(())
    }

    pub fn param_text(&mut self, id: u32, normalized: f32) -> Option<String> {
        let c = self.controller.as_ref()?;
        let mut s: String128 = [0; 128];
        (unsafe { c.getParamStringByValue(id, normalized as f64, &mut s) } == OK).then(|| from_utf16(&s))
    }

    pub fn has_editor(&mut self) -> bool {
        self.controller.is_some()
    }

    pub fn is_editor_open(&self) -> bool {
        self.view.is_some()
    }

    fn platform_type() -> FIDString {
        #[cfg(target_os = "macos")]
        {
            kPlatformTypeNSView
        }
        #[cfg(not(target_os = "macos"))]
        {
            kPlatformTypeHWND
        }
    }

    pub fn open_editor(&mut self, title: &str) -> Result<()> {
        let controller = self.controller.as_ref().ok_or(HostError::NoEditor)?;
        let raw = unsafe { controller.createView(c"editor".as_ptr()) };
        let view = unsafe { ComPtr::from_raw(raw) }.ok_or(HostError::NoEditor)?;
        let platform = Self::platform_type();
        if unsafe { view.isPlatformTypeSupported(platform) } != OK {
            return Err(HostError::Editor("view does not support this platform".into()));
        }
        let mut rect: ViewRect = unsafe { std::mem::zeroed() };
        unsafe { view.getSize(&mut rect) };
        let (mut w, mut h) = rect_size(&rect);
        if w == 0 || h == 0 {
            (w, h) = (640, 480);
        }
        let resizable = unsafe { view.canResize() } == OK;
        let window = EditorWindow::new(title, w, h, resizable)?;
        *self.frame.window.lock() = Some(window.raw());
        *self.frame.size.lock() = (w, h);
        let frame_ptr = self.frame.as_com_ref::<IPlugFrame>().map_or(std::ptr::null_mut(), |r| r.as_ptr());
        unsafe {
            view.setFrame(frame_ptr);
            #[cfg(windows)]
            if let Some(scale) = view.cast::<IPlugViewContentScaleSupport>() {
                scale.setContentScaleFactor(window.scale_factor() as f32);
            }
            if view.attached(window.parent_ptr(), platform) != OK {
                view.setFrame(std::ptr::null_mut());
                *self.frame.window.lock() = None;
                return Err(HostError::Editor("IPlugView::attached failed".into()));
            }
            // Scale changes may have resized the view.
            let mut rect: ViewRect = std::mem::zeroed();
            if view.getSize(&mut rect) == OK {
                let (nw, nh) = rect_size(&rect);
                if nw > 0 && nh > 0 && (nw, nh) != (w, h) {
                    *self.frame.size.lock() = (nw, nh);
                    window.set_client_size(nw, nh);
                }
            }
        }
        window.show();
        self.view = Some(view);
        self.window = Some(window);
        Ok(())
    }

    pub fn close_editor(&mut self) {
        if let Some(view) = self.view.take() {
            unsafe {
                view.removed();
                view.setFrame(std::ptr::null_mut());
            }
        }
        *self.frame.window.lock() = None;
        self.window = None;
    }

    pub fn idle(&mut self, state: &SharedState) {
        // Host → controller: automation and set_param values, so the GUI
        // follows them.
        while let Ok((id, v)) = self.to_controller.pop() {
            if let Some(c) = &self.controller {
                unsafe { c.setParamNormalized(id, v) };
            }
        }
        let flags = self.handler.restart_flags.swap(0, Ordering::AcqRel);
        if flags != 0 {
            use RestartFlags_::*;
            if flags & kParamValuesChanged as i32 != 0 {
                if let Some(c) = &self.controller {
                    for &id in &self.param_ids {
                        state.params.set(id, unsafe { c.getParamNormalized(id) } as f32);
                    }
                }
            }
            if flags & kLatencyChanged as i32 != 0 {
                state.latency.store(unsafe { self.processor.getLatencySamples() }, Ordering::Relaxed);
                state.latency_changed.store(true, Ordering::Release);
                state.needs_restart.store(true, Ordering::Release);
            }
            if flags & (kReloadComponent as i32 | kIoChanged as i32) != 0 {
                state.needs_restart.store(true, Ordering::Release);
            }
        }
        if self.handler.dirty.swap(false, Ordering::AcqRel) {
            state.state_dirty.store(true, Ordering::Release);
        }
        if self.handler.open_requested.swap(false, Ordering::AcqRel) && self.view.is_none() {
            let title = format!("{} — {}", state.hosted.name, state.hosted.vendor);
            if let Err(e) = self.open_editor(&title) {
                log::warn!("{}: requested editor failed to open: {e}", state.hosted.name);
            }
        }
        self.poll_editor();
    }

    fn poll_editor(&mut self) {
        let closed = self.window.as_ref().is_some_and(|w| w.user_closed());
        if closed {
            self.close_editor();
            return;
        }
        let (Some(view), Some(window)) = (&self.view, &self.window) else { return };
        let (cw, ch) = window.client_size();
        let (fw, fh) = *self.frame.size.lock();
        if cw == 0 || ch == 0 || (cw, ch) == (fw, fh) {
            return;
        }
        unsafe {
            if view.canResize() == OK {
                let mut r = ViewRect { left: 0, top: 0, right: cw as i32, bottom: ch as i32 };
                view.checkSizeConstraint(&mut r);
                let (nw, nh) = rect_size(&r);
                *self.frame.size.lock() = (nw, nh);
                view.onSize(&mut r);
                if (nw, nh) != (cw, ch) {
                    window.set_client_size(nw, nh);
                }
            } else {
                window.set_client_size(fw, fh);
            }
        }
    }
}

fn audio_t() -> MediaType {
    audio()
}

fn rect_size(r: &ViewRect) -> (u32, u32) {
    ((r.right - r.left).max(0) as u32, (r.bottom - r.top).max(0) as u32)
}

/// Our container: `"AIV3" | u32 version | u32 len | component | u32 len |
/// controller`. Data without the magic is treated as a bare component
/// state (e.g. a `.vstpreset` chunk).
fn split_state(data: &[u8]) -> Option<(&[u8], &[u8])> {
    if data.len() < 8 || &data[..4] != STATE_MAGIC {
        return Some((data, &[]));
    }
    let rd = |at: usize| -> Option<u32> { data.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
    let a_len = rd(8)? as usize;
    let a = data.get(12..12 + a_len)?;
    let b_len = rd(12 + a_len)? as usize;
    let b = data.get(16 + a_len..16 + a_len + b_len)?;
    Some((a, b))
}

impl Drop for Vst3Main {
    fn drop(&mut self) {
        self.close_editor();
        unsafe {
            if self.active {
                self.component.setActive(0);
            }
            if let Some((a, b)) = self.connections.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
                if self.separate_controller {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
        let _ = &self.host_app;
    }
}

impl Vst3Audio {
    fn configure(&mut self, in_ch: &[usize], out_ch: &[usize], max_frames: usize, sample_rate: f64) {
        self.sample_rate = sample_rate;
        self.in_scratch = BusScratch::new(in_ch, max_frames);
        self.out_scratch = BusScratch::new(out_ch, max_frames);
        self.in_bufs = (0..in_ch.len())
            .map(|i| AudioBusBuffers {
                numChannels: in_ch[i] as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 { channelBuffers32: self.in_scratch.ptrs(i) },
            })
            .collect();
        self.out_bufs = (0..out_ch.len())
            .map(|i| AudioBusBuffers {
                numChannels: out_ch[i] as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 { channelBuffers32: self.out_scratch.ptrs(i) },
            })
            .collect();
    }

    pub fn reset(&mut self) {
        if self.processing {
            unsafe {
                self.processor.setProcessing(0);
                self.processor.setProcessing(1);
            }
        }
    }

    #[inline]
    fn host_param(&mut self, state: &SharedState, id: u32, offset: i32, value: f64) {
        self.in_changes.add(id, offset, value);
        state.params.set(id, value as f32);
        let _ = self.to_controller.push((id, value));
    }

    fn midi_param(&self, channel: u8, index: usize) -> Option<u32> {
        let id = *self.midi_map.get((channel as usize & 15) * MIDI_MAP_STRIDE + index)?;
        (id != NO_PARAM).then_some(id)
    }

    pub fn process(&mut self, ctx: &mut ProcessContext<'_>, state: &SharedState, pending: &mut PendingParams) -> bool {
        let frames = ctx.buffer.frames().min(self.in_scratch.max_frames().max(self.out_scratch.max_frames()));
        if frames == 0 {
            return true;
        }

        // ── Parameters ──
        self.in_changes.clear();
        self.out_changes.clear();
        while let Ok((id, v)) = self.from_gui.pop() {
            self.in_changes.add(id, 0, v);
            state.params.set(id, v as f32);
        }
        let infos = state.params.infos();
        pending.drain(|index, v| {
            let id = infos[index].id;
            self.in_changes.add(id, 0, v as f64);
            let _ = self.to_controller.push((id, v as f64));
        });

        // ── Events ──
        self.in_events.clear();
        self.out_events.clear();
        for e in ctx.events {
            let offset = e.offset.min(frames as u32 - 1) as i32;
            match e.kind {
                EventKind::Param { id, value } => self.host_param(state, id, offset, value as f64),
                EventKind::NoteOn { channel, key, velocity, note_id } => {
                    let ev = NoteOnEvent { channel: channel as i16, pitch: key as i16, tuning: 0.0, velocity, length: 0, noteId: note_id };
                    self.in_events.push(make_event(offset, Event_::EventTypes_::kNoteOnEvent as u16, Event__type0 { noteOn: ev }));
                }
                EventKind::NoteOff { channel, key, velocity, note_id } => {
                    let ev = NoteOffEvent { channel: channel as i16, pitch: key as i16, velocity, noteId: note_id, tuning: 0.0 };
                    self.in_events.push(make_event(offset, Event_::EventTypes_::kNoteOffEvent as u16, Event__type0 { noteOff: ev }));
                }
                EventKind::NoteExpression { note_id, key, expr, value } => {
                    if expr == Expression::Pressure {
                        let ev = PolyPressureEvent { channel: 0, pitch: key as i16, pressure: value, noteId: note_id };
                        self.in_events.push(make_event(offset, Event_::EventTypes_::kPolyPressureEvent as u16, Event__type0 { polyPressure: ev }));
                    } else {
                        use NoteExpressionTypeIDs_::*;
                        let (type_id, v) = match expr {
                            Expression::Tuning => (kTuningTypeID, 0.5 + value as f64 / 240.0),
                            Expression::Pan => (kPanTypeID, (value as f64 + 1.0) * 0.5),
                            Expression::Brightness => (kBrightnessTypeID, value as f64),
                            Expression::Volume => (kVolumeTypeID, value as f64 * 0.25),
                            Expression::Pressure => unreachable!(),
                        };
                        let ev = NoteExpressionValueEvent { typeId: type_id as u32, noteId: note_id, value: v.clamp(0.0, 1.0) };
                        self.in_events.push(make_event(
                            offset,
                            Event_::EventTypes_::kNoteExpressionValueEvent as u16,
                            Event__type0 { noteExpressionValue: ev },
                        ));
                    }
                }
                // VST3 has no MIDI CC events: controllers map them to
                // parameters through IMidiMapping.
                EventKind::ControlChange { channel, cc, value } => {
                    if let Some(id) = self.midi_param(channel, cc as usize) {
                        self.host_param(state, id, offset, value as f64);
                    }
                }
                EventKind::ChannelPressure { channel, value } => {
                    if let Some(id) = self.midi_param(channel, 128) {
                        self.host_param(state, id, offset, value as f64);
                    }
                }
                EventKind::PitchBend { channel, value } => {
                    if let Some(id) = self.midi_param(channel, 129) {
                        self.host_param(state, id, offset, ((value as f64) + 1.0) * 0.5);
                    }
                }
                EventKind::AllNotesOff => {
                    for ch in 0..16u8 {
                        if let Some(id) = self.midi_param(ch, 123) {
                            self.host_param(state, id, offset, 0.0);
                        }
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

        let t = ctx.transport;
        let c = &mut *self.context;
        {
            use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::*;
            let mut st = kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid;
            if t.playing {
                st |= kPlaying;
            }
            if t.recording {
                st |= kRecording;
            }
            if let Some((a, b)) = t.loop_range_beats {
                st |= kCycleValid | kCycleActive;
                c.cycleStartMusic = a;
                c.cycleEndMusic = b;
            }
            c.state = st as u32;
        }
        c.sampleRate = ctx.sample_rate;
        c.projectTimeSamples = t.position_samples;
        c.projectTimeMusic = t.position_beats;
        c.barPositionMusic = bar_start_beats(t);
        c.tempo = t.bpm;
        c.timeSigNumerator = t.time_sig.0 as i32;
        c.timeSigDenominator = t.time_sig.1 as i32;

        let in_changes = self.in_changes.as_com_ref::<IParameterChanges>().map_or(std::ptr::null_mut(), |r| r.as_ptr());
        let out_changes = self.out_changes.as_com_ref::<IParameterChanges>().map_or(std::ptr::null_mut(), |r| r.as_ptr());
        let in_events = if self.has_event_input {
            self.in_events.as_com_ref::<IEventList>().map_or(std::ptr::null_mut(), |r| r.as_ptr())
        } else {
            std::ptr::null_mut()
        };
        let out_events = self.out_events.as_com_ref::<IEventList>().map_or(std::ptr::null_mut(), |r| r.as_ptr());
        let mut data = ProcessData {
            processMode: ProcessModes_::kRealtime as i32,
            symbolicSampleSize: SymbolicSampleSizes_::kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.in_bufs.len() as i32,
            numOutputs: self.out_bufs.len() as i32,
            inputs: if self.in_bufs.is_empty() { std::ptr::null_mut() } else { self.in_bufs.as_mut_ptr() },
            outputs: if self.out_bufs.is_empty() { std::ptr::null_mut() } else { self.out_bufs.as_mut_ptr() },
            inputParameterChanges: in_changes,
            outputParameterChanges: out_changes,
            inputEvents: in_events,
            outputEvents: out_events,
            processContext: &mut *self.context,
        };

        unsafe {
            if !self.processing {
                self.processor.setProcessing(1);
                self.processing = true;
            }
        }
        let r = unsafe { self.processor.process(&mut data) };

        self.out_scratch.write_to(0, ctx.buffer);

        // Parameter changes reported by the plugin.
        let to_controller = &mut self.to_controller;
        self.out_changes.for_each_last(|id, v| {
            state.params.set(id, v as f32);
            let _ = to_controller.push((id, v));
        });
        r == OK
    }
}

fn make_event(offset: i32, kind: u16, body: Event__type0) -> Event {
    Event { busIndex: 0, sampleOffset: offset, ppqPosition: 0.0, flags: Event_::EventFlags_::kIsLive as u16, r#type: kind, __field0: body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_container() {
        let mut data = Vec::new();
        data.extend_from_slice(STATE_MAGIC);
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&[1, 2, 3]);
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&[9, 9]);
        assert_eq!(split_state(&data), Some((&[1u8, 2, 3][..], &[9u8, 9][..])));
        assert_eq!(split_state(&[5, 6]), Some((&[5u8, 6][..], &[][..])));
        assert_eq!(split_state(&data[..10]), None);
    }
}
