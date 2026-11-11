//! Deterministic test plugin for `aitrack-host`.
//!
//! One library exports both a CLAP plugin (`clap_entry`) and a split
//! (processor + controller) VST3 plugin (`GetPluginFactory`). Both are a
//! stereo gain with one parameter, report 32 samples of latency and save
//! their gain as state.
//!
//! - CLAP param 0 "Gain": plain 0..=2, default 1; applied sample-accurately.
//!   A note-on with key 127 makes the plugin emit a gesture + param value
//!   (gain 0.5) as output events, simulating an edit in its GUI.
//! - VST3 param 0 "Gain": normalised 0..=1 (gain = 2 × value), default 0.5.

#![allow(non_snake_case, non_upper_case_globals, clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void, CStr};
use std::sync::atomic::{AtomicU64, Ordering};

const LATENCY: u32 = 32;
const STATE_MAGIC: &[u8; 4] = b"TGN1";

fn copy_cstr(src: &str, dst: &mut [c_char]) {
    let n = src.len().min(dst.len().saturating_sub(1));
    for (d, s) in dst.iter_mut().zip(src.bytes().take(n)) {
        *d = s as c_char;
    }
    if !dst.is_empty() {
        dst[n] = 0;
    }
}

fn encode_state(gain: f64) -> Vec<u8> {
    let mut v = STATE_MAGIC.to_vec();
    v.extend_from_slice(&gain.to_le_bytes());
    v
}

fn decode_state(data: &[u8]) -> Option<f64> {
    if data.len() != 12 || &data[..4] != STATE_MAGIC {
        return None;
    }
    Some(f64::from_le_bytes(data[4..12].try_into().ok()?))
}

// ═════════════════════════════════════ CLAP ═════════════════════════════════

mod clap {
    use super::*;
    use clap_sys::entry::clap_plugin_entry;
    use clap_sys::events::*;
    use clap_sys::ext::audio_ports::*;
    use clap_sys::ext::latency::*;
    use clap_sys::ext::params::*;
    use clap_sys::ext::state::*;
    use clap_sys::factory::plugin_factory::*;
    use clap_sys::host::clap_host;
    use clap_sys::id::clap_id;
    use clap_sys::plugin::*;
    use clap_sys::process::*;
    use clap_sys::stream::*;
    use clap_sys::version::CLAP_VERSION;

    pub const PLUGIN_ID: &CStr = c"org.aitrack.test-gain";

    struct Sync<T>(T);
    unsafe impl<T> std::marker::Sync for Sync<T> {}

    static FEATURES: Sync<[*const c_char; 3]> = Sync([c"audio-effect".as_ptr(), c"stereo".as_ptr(), std::ptr::null()]);

    static DESCRIPTOR: Sync<clap_plugin_descriptor> = Sync(clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: PLUGIN_ID.as_ptr(),
        name: c"AI-Track Test Gain".as_ptr(),
        vendor: c"AI-Track".as_ptr(),
        url: c"".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"1.0.0".as_ptr(),
        description: c"Test gain".as_ptr(),
        features: &FEATURES.0 as *const _ as *const *const c_char,
    });

    #[repr(C)]
    struct Plugin {
        plugin: clap_plugin,
        _host: *const clap_host,
        gain: AtomicU64,
    }

    unsafe fn this<'a>(p: *const clap_plugin) -> &'a Plugin {
        &*((*p).plugin_data as *const Plugin)
    }

    impl Plugin {
        fn gain(&self) -> f64 {
            f64::from_bits(self.gain.load(Ordering::Relaxed))
        }
        fn set_gain(&self, g: f64) {
            self.gain.store(g.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
        }
    }

    unsafe extern "C" fn init(_p: *const clap_plugin) -> bool {
        true
    }
    unsafe extern "C" fn destroy(p: *const clap_plugin) {
        drop(Box::from_raw((*p).plugin_data as *mut Plugin));
    }
    unsafe extern "C" fn activate(_p: *const clap_plugin, _sr: f64, _min: u32, _max: u32) -> bool {
        true
    }
    unsafe extern "C" fn deactivate(_p: *const clap_plugin) {}
    unsafe extern "C" fn start_processing(_p: *const clap_plugin) -> bool {
        true
    }
    unsafe extern "C" fn stop_processing(_p: *const clap_plugin) {}
    unsafe extern "C" fn reset(_p: *const clap_plugin) {}
    unsafe extern "C" fn on_main_thread(_p: *const clap_plugin) {}

    unsafe fn apply_param_event(plugin: &Plugin, hdr: *const clap_event_header) {
        if (*hdr).space_id == CLAP_CORE_EVENT_SPACE_ID && (*hdr).type_ == CLAP_EVENT_PARAM_VALUE {
            let ev = &*(hdr as *const clap_event_param_value);
            if ev.param_id == 0 {
                plugin.set_gain(ev.value);
            }
        }
    }

    unsafe fn push_out(out: *const clap_output_events, hdr: *const clap_event_header) {
        if let Some(try_push) = (*out).try_push {
            try_push(out, hdr);
        }
    }

    unsafe extern "C" fn process(p: *const clap_plugin, process: *const clap_process) -> clap_process_status {
        let plugin = this(p);
        let pr = &*process;
        let frames = pr.frames_count as usize;
        if pr.audio_inputs_count < 1 || pr.audio_outputs_count < 1 {
            return CLAP_PROCESS_ERROR;
        }
        let inp = &*pr.audio_inputs;
        let out = &*pr.audio_outputs;
        let chans = inp.channel_count.min(out.channel_count) as usize;
        let in_events = &*pr.in_events;
        let n_events = (in_events.size.unwrap())(in_events);
        let mut next_ev = 0u32;
        for i in 0..frames {
            while next_ev < n_events {
                let hdr = (in_events.get.unwrap())(in_events, next_ev);
                if (*hdr).time as usize > i {
                    break;
                }
                apply_param_event(plugin, hdr);
                if (*hdr).space_id == CLAP_CORE_EVENT_SPACE_ID && (*hdr).type_ == CLAP_EVENT_NOTE_ON {
                    let note = &*(hdr as *const clap_event_note);
                    if note.key == 127 {
                        // Simulated GUI edit: gesture + value + gesture end.
                        let mk = |t| clap_event_header {
                            size: std::mem::size_of::<clap_event_param_gesture>() as u32,
                            time: 0,
                            space_id: CLAP_CORE_EVENT_SPACE_ID,
                            type_: t,
                            flags: 0,
                        };
                        let begin = clap_event_param_gesture { header: mk(CLAP_EVENT_PARAM_GESTURE_BEGIN), param_id: 0 as clap_id };
                        push_out(pr.out_events, &begin.header);
                        let val = clap_event_param_value {
                            header: clap_event_header {
                                size: std::mem::size_of::<clap_event_param_value>() as u32,
                                time: 0,
                                space_id: CLAP_CORE_EVENT_SPACE_ID,
                                type_: CLAP_EVENT_PARAM_VALUE,
                                flags: 0,
                            },
                            param_id: 0,
                            cookie: std::ptr::null_mut(),
                            note_id: -1,
                            port_index: -1,
                            channel: -1,
                            key: -1,
                            value: 0.5,
                        };
                        push_out(pr.out_events, &val.header);
                        plugin.set_gain(0.5);
                        let end = clap_event_param_gesture { header: mk(CLAP_EVENT_PARAM_GESTURE_END), param_id: 0 };
                        push_out(pr.out_events, &end.header);
                    }
                }
                next_ev += 1;
            }
            let g = plugin.gain() as f32;
            for ch in 0..chans {
                let src = *inp.data32.add(ch);
                let dst = *out.data32.add(ch);
                *dst.add(i) = *src.add(i) * g;
            }
        }
        while next_ev < n_events {
            apply_param_event(plugin, (in_events.get.unwrap())(in_events, next_ev));
            next_ev += 1;
        }
        CLAP_PROCESS_CONTINUE
    }

    // ── audio ports ──
    unsafe extern "C" fn ports_count(_p: *const clap_plugin, _is_input: bool) -> u32 {
        1
    }
    unsafe extern "C" fn ports_get(_p: *const clap_plugin, index: u32, is_input: bool, info: *mut clap_audio_port_info) -> bool {
        if index != 0 {
            return false;
        }
        let info = &mut *info;
        info.id = 0;
        copy_cstr(if is_input { "In" } else { "Out" }, &mut info.name);
        info.flags = CLAP_AUDIO_PORT_IS_MAIN;
        info.channel_count = 2;
        info.port_type = CLAP_PORT_STEREO.as_ptr();
        info.in_place_pair = 0;
        true
    }
    static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports { count: Some(ports_count), get: Some(ports_get) };

    // ── params ──
    unsafe extern "C" fn params_count(_p: *const clap_plugin) -> u32 {
        1
    }
    unsafe extern "C" fn params_get_info(_p: *const clap_plugin, index: u32, info: *mut clap_param_info) -> bool {
        if index != 0 {
            return false;
        }
        let info = &mut *info;
        info.id = 0;
        info.flags = CLAP_PARAM_IS_AUTOMATABLE;
        info.cookie = std::ptr::null_mut();
        copy_cstr("Gain", &mut info.name);
        copy_cstr("", &mut info.module);
        info.min_value = 0.0;
        info.max_value = 2.0;
        info.default_value = 1.0;
        true
    }
    unsafe extern "C" fn params_get_value(p: *const clap_plugin, id: clap_id, out: *mut f64) -> bool {
        if id != 0 {
            return false;
        }
        *out = this(p).gain();
        true
    }
    unsafe extern "C" fn params_value_to_text(_p: *const clap_plugin, id: clap_id, value: f64, buf: *mut c_char, cap: u32) -> bool {
        if id != 0 || cap == 0 {
            return false;
        }
        let s = format!("{value:.2}x");
        copy_cstr(&s, std::slice::from_raw_parts_mut(buf, cap as usize));
        true
    }
    unsafe extern "C" fn params_text_to_value(_p: *const clap_plugin, id: clap_id, text: *const c_char, out: *mut f64) -> bool {
        if id != 0 {
            return false;
        }
        match CStr::from_ptr(text).to_str().ok().and_then(|s| s.trim_end_matches('x').parse::<f64>().ok()) {
            Some(v) => {
                *out = v;
                true
            }
            None => false,
        }
    }
    unsafe extern "C" fn params_flush(p: *const clap_plugin, input: *const clap_input_events, _out: *const clap_output_events) {
        let plugin = this(p);
        let ev = &*input;
        for i in 0..(ev.size.unwrap())(ev) {
            apply_param_event(plugin, (ev.get.unwrap())(ev, i));
        }
    }
    static PARAMS: clap_plugin_params = clap_plugin_params {
        count: Some(params_count),
        get_info: Some(params_get_info),
        get_value: Some(params_get_value),
        value_to_text: Some(params_value_to_text),
        text_to_value: Some(params_text_to_value),
        flush: Some(params_flush),
    };

    // ── state ──
    unsafe extern "C" fn state_save(p: *const clap_plugin, stream: *const clap_ostream) -> bool {
        let data = encode_state(this(p).gain());
        let mut written = 0usize;
        while written < data.len() {
            let n = ((*stream).write.unwrap())(stream, data[written..].as_ptr() as *const c_void, (data.len() - written) as u64);
            if n <= 0 {
                return false;
            }
            written += n as usize;
        }
        true
    }
    unsafe extern "C" fn state_load(p: *const clap_plugin, stream: *const clap_istream) -> bool {
        let mut data = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let n = ((*stream).read.unwrap())(stream, buf.as_mut_ptr() as *mut c_void, buf.len() as u64);
            if n < 0 {
                return false;
            }
            if n == 0 {
                break;
            }
            data.extend_from_slice(&buf[..n as usize]);
        }
        match decode_state(&data) {
            Some(g) => {
                this(p).set_gain(g);
                true
            }
            None => false,
        }
    }
    static STATE: clap_plugin_state = clap_plugin_state { save: Some(state_save), load: Some(state_load) };

    // ── latency ──
    unsafe extern "C" fn latency_get(_p: *const clap_plugin) -> u32 {
        LATENCY
    }
    static LATENCY_EXT: clap_plugin_latency = clap_plugin_latency { get: Some(latency_get) };

    unsafe extern "C" fn get_extension(_p: *const clap_plugin, id: *const c_char) -> *const c_void {
        let id = CStr::from_ptr(id);
        if id == CLAP_EXT_AUDIO_PORTS {
            &AUDIO_PORTS as *const _ as *const c_void
        } else if id == CLAP_EXT_PARAMS {
            &PARAMS as *const _ as *const c_void
        } else if id == CLAP_EXT_STATE {
            &STATE as *const _ as *const c_void
        } else if id == CLAP_EXT_LATENCY {
            &LATENCY_EXT as *const _ as *const c_void
        } else {
            std::ptr::null()
        }
    }

    // ── factory / entry ──
    unsafe extern "C" fn factory_count(_f: *const clap_plugin_factory) -> u32 {
        1
    }
    unsafe extern "C" fn factory_descriptor(_f: *const clap_plugin_factory, index: u32) -> *const clap_plugin_descriptor {
        if index == 0 {
            &DESCRIPTOR.0
        } else {
            std::ptr::null()
        }
    }
    unsafe extern "C" fn factory_create(_f: *const clap_plugin_factory, host: *const clap_host, id: *const c_char) -> *const clap_plugin {
        if CStr::from_ptr(id) != PLUGIN_ID {
            return std::ptr::null();
        }
        let plugin = Box::into_raw(Box::new(Plugin {
            plugin: clap_plugin {
                desc: &DESCRIPTOR.0,
                plugin_data: std::ptr::null_mut(),
                init: Some(init),
                destroy: Some(destroy),
                activate: Some(activate),
                deactivate: Some(deactivate),
                start_processing: Some(start_processing),
                stop_processing: Some(stop_processing),
                reset: Some(reset),
                process: Some(process),
                get_extension: Some(get_extension),
                on_main_thread: Some(on_main_thread),
            },
            _host: host,
            gain: AtomicU64::new(1.0f64.to_bits()),
        }));
        (*plugin).plugin.plugin_data = plugin as *mut c_void;
        &(*plugin).plugin
    }
    static FACTORY: clap_plugin_factory = clap_plugin_factory {
        get_plugin_count: Some(factory_count),
        get_plugin_descriptor: Some(factory_descriptor),
        create_plugin: Some(factory_create),
    };

    unsafe extern "C" fn entry_init(_path: *const c_char) -> bool {
        true
    }
    unsafe extern "C" fn entry_deinit() {}
    unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
        if CStr::from_ptr(id) == CLAP_PLUGIN_FACTORY_ID {
            &FACTORY as *const _ as *const c_void
        } else {
            std::ptr::null()
        }
    }

    #[no_mangle]
    #[used]
    pub static clap_entry: clap_plugin_entry =
        clap_plugin_entry { clap_version: CLAP_VERSION, init: Some(entry_init), deinit: Some(entry_deinit), get_factory: Some(entry_get_factory) };
}

// ═════════════════════════════════════ VST3 ═════════════════════════════════

mod vst {
    use super::*;
    use std::slice;
    use vst3::Steinberg::Vst::*;
    use vst3::Steinberg::*;
    use vst3::{uid, Class, ComRef, ComWrapper};

    const PROCESSOR_CID: TUID = uid(0x41495452, 0x5447_4149, 0x4E50_524F, 0x43000001);
    const CONTROLLER_CID: TUID = uid(0x41495452, 0x5447_4149, 0x4E43_544C, 0x52000001);

    fn copy_w(src: &str, dst: &mut [TChar]) {
        let mut n = 0;
        for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
            *d = s as TChar;
            n += 1;
        }
        let end = n.min(dst.len() - 1);
        dst[end] = 0;
    }

    unsafe fn read_stream(stream: *mut IBStream) -> Option<Vec<u8>> {
        let s = ComRef::from_raw(stream)?;
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let mut n = 0;
            if s.read(buf.as_mut_ptr() as *mut c_void, buf.len() as i32, &mut n) != kResultOk || n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        Some(out)
    }

    unsafe fn write_stream(stream: *mut IBStream, data: &[u8]) -> tresult {
        let Some(s) = ComRef::from_raw(stream) else { return kInvalidArgument };
        let mut n = 0;
        s.write(data.as_ptr() as *mut c_void, data.len() as i32, &mut n)
    }

    struct Processor {
        /// Normalised gain parameter (gain = 2 × value).
        value: AtomicU64,
    }

    impl Class for Processor {
        type Interfaces = (IComponent, IAudioProcessor);
    }

    impl Processor {
        fn value(&self) -> f64 {
            f64::from_bits(self.value.load(Ordering::Relaxed))
        }
        fn set(&self, v: f64) {
            self.value.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    impl IPluginBaseTrait for Processor {
        unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
            kResultOk
        }
        unsafe fn terminate(&self) -> tresult {
            kResultOk
        }
    }

    impl IComponentTrait for Processor {
        unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
            *class_id = CONTROLLER_CID;
            kResultOk
        }
        unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
            kResultOk
        }
        unsafe fn getBusCount(&self, media: MediaType, _dir: BusDirection) -> i32 {
            if media == MediaTypes_::kAudio as MediaType {
                1
            } else {
                0
            }
        }
        unsafe fn getBusInfo(&self, media: MediaType, dir: BusDirection, index: i32, bus: *mut BusInfo) -> tresult {
            if media != MediaTypes_::kAudio as MediaType || index != 0 {
                return kInvalidArgument;
            }
            let bus = &mut *bus;
            bus.mediaType = media;
            bus.direction = dir;
            bus.channelCount = 2;
            copy_w(if dir == BusDirections_::kInput as BusDirection { "In" } else { "Out" }, &mut bus.name);
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
            kResultOk
        }
        unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
            kNotImplemented
        }
        unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
            kResultOk
        }
        unsafe fn setActive(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn setState(&self, state: *mut IBStream) -> tresult {
            match read_stream(state).and_then(|d| decode_state(&d)) {
                Some(v) => {
                    self.set(v);
                    kResultOk
                }
                None => kResultFalse,
            }
        }
        unsafe fn getState(&self, state: *mut IBStream) -> tresult {
            write_stream(state, &encode_state(self.value()))
        }
    }

    impl IAudioProcessorTrait for Processor {
        unsafe fn setBusArrangements(&self, inputs: *mut SpeakerArrangement, n_in: i32, outputs: *mut SpeakerArrangement, n_out: i32) -> tresult {
            if n_in == 1 && n_out == 1 && *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo {
                kResultTrue
            } else {
                kResultFalse
            }
        }
        unsafe fn getBusArrangement(&self, _dir: BusDirection, index: i32, arr: *mut SpeakerArrangement) -> tresult {
            if index != 0 {
                return kInvalidArgument;
            }
            *arr = SpeakerArr::kStereo;
            kResultOk
        }
        unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
            if size == SymbolicSampleSizes_::kSample32 as i32 {
                kResultOk
            } else {
                kNotImplemented
            }
        }
        unsafe fn getLatencySamples(&self) -> u32 {
            LATENCY
        }
        unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
            kResultOk
        }
        unsafe fn setProcessing(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn process(&self, data: *mut ProcessData) -> tresult {
            let d = &*data;
            // Collect the gain parameter's points (sample-accurate).
            let mut points: Vec<(i32, f64)> = Vec::new();
            if let Some(changes) = ComRef::from_raw(d.inputParameterChanges) {
                for i in 0..changes.getParameterCount() {
                    let Some(q) = ComRef::from_raw(changes.getParameterData(i)) else { continue };
                    if q.getParameterId() != 0 {
                        continue;
                    }
                    for p in 0..q.getPointCount() {
                        let (mut o, mut v) = (0, 0.0);
                        if q.getPoint(p, &mut o, &mut v) == kResultOk {
                            points.push((o, v));
                        }
                    }
                }
            }
            if d.numInputs < 1 || d.numOutputs < 1 {
                for &(_, v) in &points {
                    self.set(v);
                }
                return kResultOk;
            }
            let frames = d.numSamples as usize;
            let inb = &*d.inputs;
            let outb = &*d.outputs;
            let chans = inb.numChannels.min(outb.numChannels) as usize;
            let ins = slice::from_raw_parts(inb.__field0.channelBuffers32, chans);
            let outs = slice::from_raw_parts(outb.__field0.channelBuffers32, chans);
            let mut next = 0;
            for i in 0..frames {
                while next < points.len() && points[next].0 as usize <= i {
                    self.set(points[next].1);
                    next += 1;
                }
                let g = (self.value() * 2.0) as f32;
                for ch in 0..chans {
                    *outs[ch].add(i) = *ins[ch].add(i) * g;
                }
            }
            for &(_, v) in &points[next..] {
                self.set(v);
            }
            kResultOk
        }
        unsafe fn getTailSamples(&self) -> u32 {
            0
        }
    }

    struct Controller {
        value: AtomicU64,
    }

    impl Class for Controller {
        type Interfaces = (IEditController,);
    }

    impl IPluginBaseTrait for Controller {
        unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
            kResultOk
        }
        unsafe fn terminate(&self) -> tresult {
            kResultOk
        }
    }

    impl IEditControllerTrait for Controller {
        unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
            match read_stream(state).and_then(|d| decode_state(&d)) {
                Some(v) => {
                    self.value.store(v.to_bits(), Ordering::Relaxed);
                    kResultOk
                }
                None => kResultFalse,
            }
        }
        unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
            kResultOk
        }
        unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
            kResultOk
        }
        unsafe fn getParameterCount(&self) -> i32 {
            1
        }
        unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
            if index != 0 {
                return kInvalidArgument;
            }
            let info = &mut *info;
            info.id = 0;
            copy_w("Gain", &mut info.title);
            copy_w("Gain", &mut info.shortTitle);
            copy_w("x", &mut info.units);
            info.stepCount = 0;
            info.defaultNormalizedValue = 0.5;
            info.unitId = 0;
            info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
            kResultOk
        }
        unsafe fn getParamStringByValue(&self, id: u32, v: f64, s: *mut String128) -> tresult {
            if id != 0 {
                return kInvalidArgument;
            }
            copy_w(&format!("{:.2}x", v * 2.0), &mut *s);
            kResultOk
        }
        unsafe fn getParamValueByString(&self, _id: u32, _s: *mut TChar, _v: *mut f64) -> tresult {
            kNotImplemented
        }
        unsafe fn normalizedParamToPlain(&self, _id: u32, v: f64) -> f64 {
            v * 2.0
        }
        unsafe fn plainParamToNormalized(&self, _id: u32, v: f64) -> f64 {
            v / 2.0
        }
        unsafe fn getParamNormalized(&self, id: u32) -> f64 {
            if id == 0 {
                f64::from_bits(self.value.load(Ordering::Relaxed))
            } else {
                0.0
            }
        }
        unsafe fn setParamNormalized(&self, id: u32, v: f64) -> tresult {
            if id != 0 {
                return kInvalidArgument;
            }
            self.value.store(v.to_bits(), Ordering::Relaxed);
            kResultOk
        }
        unsafe fn setComponentHandler(&self, _h: *mut IComponentHandler) -> tresult {
            kResultOk
        }
        unsafe fn createView(&self, _name: *const c_char) -> *mut IPlugView {
            std::ptr::null_mut()
        }
    }

    struct Factory;

    impl Class for Factory {
        type Interfaces = (IPluginFactory2,);
    }

    impl IPluginFactoryTrait for Factory {
        unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
            let info = &mut *info;
            copy_cstr("AI-Track", &mut info.vendor);
            copy_cstr("", &mut info.url);
            copy_cstr("", &mut info.email);
            info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as i32;
            kResultOk
        }
        unsafe fn countClasses(&self) -> i32 {
            2
        }
        unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
            let info = &mut *info;
            info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
            match index {
                0 => {
                    info.cid = PROCESSOR_CID;
                    copy_cstr("Audio Module Class", &mut info.category);
                }
                1 => {
                    info.cid = CONTROLLER_CID;
                    copy_cstr("Component Controller Class", &mut info.category);
                }
                _ => return kInvalidArgument,
            }
            copy_cstr("AI-Track Test Gain", &mut info.name);
            kResultOk
        }
        unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
            let unknown = match *(cid as *const TUID) {
                PROCESSOR_CID => ComWrapper::new(Processor { value: AtomicU64::new(0.5f64.to_bits()) }).to_com_ptr::<FUnknown>(),
                CONTROLLER_CID => ComWrapper::new(Controller { value: AtomicU64::new(0.5f64.to_bits()) }).to_com_ptr::<FUnknown>(),
                _ => None,
            };
            match unknown {
                Some(u) => {
                    let p = u.as_ptr();
                    ((*(*p).vtbl).queryInterface)(p, iid as *const TUID, obj)
                }
                None => kInvalidArgument,
            }
        }
    }

    impl IPluginFactory2Trait for Factory {
        unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
            let info = &mut *info;
            info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as i32;
            match index {
                0 => {
                    info.cid = PROCESSOR_CID;
                    copy_cstr("Audio Module Class", &mut info.category);
                    copy_cstr("Fx|Tools", &mut info.subCategories);
                }
                1 => {
                    info.cid = CONTROLLER_CID;
                    copy_cstr("Component Controller Class", &mut info.category);
                }
                _ => return kInvalidArgument,
            }
            copy_cstr("AI-Track Test Gain", &mut info.name);
            copy_cstr("AI-Track", &mut info.vendor);
            copy_cstr("1.0.0", &mut info.version);
            copy_cstr("VST 3.8.0", &mut info.sdkVersion);
            kResultOk
        }
    }

    #[no_mangle]
    pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
        ComWrapper::new(Factory).to_com_ptr::<IPluginFactory>().map_or(std::ptr::null_mut(), |p| p.into_raw())
    }

    #[cfg(windows)]
    #[no_mangle]
    pub extern "system" fn InitDll() -> bool {
        true
    }

    #[cfg(windows)]
    #[no_mangle]
    pub extern "system" fn ExitDll() -> bool {
        true
    }

    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub extern "C" fn bundleEntry(_bundle: *mut c_void) -> bool {
        true
    }

    #[cfg(target_os = "macos")]
    #[no_mangle]
    pub extern "C" fn bundleExit() -> bool {
        true
    }
}
