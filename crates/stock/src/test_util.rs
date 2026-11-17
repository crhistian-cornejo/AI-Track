//! Helpers for the stock plugins' tests and benchmarks (not for the RT path).

use aitrack_dsp::{AudioBuffer, Event, EventKind, ProcessContext, Processor, Transport};

pub const SR: f64 = 48_000.0;
pub const BLOCK: usize = 128;

/// Set a parameter from its plain value (Hz, dB, ms, choice index…).
pub fn set_plain(p: &mut dyn Processor, id: u32, plain: f32) {
    let n = p.info().params.iter().find(|i| i.id == id).map(|i| i.to_normalized(plain)).unwrap_or_else(|| panic!("no param {id}"));
    p.set_param(id, n);
}

/// Read a parameter back as its plain value.
pub fn get_plain(p: &dyn Processor, id: u32) -> f32 {
    let info = p.info().params.iter().find(|i| i.id == id).unwrap_or_else(|| panic!("no param {id}"));
    info.to_plain(p.get_param(id))
}

/// Run `n` frames of a stereo signal through `p` in `BLOCK`-sized blocks
/// (after `prepare(SR, BLOCK)`), returning the outputs.
pub fn process_stereo(p: &mut dyn Processor, input: impl FnMut(usize) -> (f32, f32), n: usize) -> (Vec<f32>, Vec<f32>) {
    p.prepare(SR, BLOCK);
    process_stereo_prepared(p, input, n, &Transport::default())
}

/// Like [`process_stereo`] without calling `prepare` first.
pub fn process_stereo_prepared(p: &mut dyn Processor, mut input: impl FnMut(usize) -> (f32, f32), n: usize, transport: &Transport) -> (Vec<f32>, Vec<f32>) {
    let mut buf = AudioBuffer::new(2, BLOCK);
    let mut out_l = Vec::with_capacity(n);
    let mut out_r = Vec::with_capacity(n);
    let mut pos = 0;
    let mut tr = *transport;
    while pos < n {
        let len = BLOCK.min(n - pos);
        buf.set_frames(len);
        {
            let (l, r) = buf.stereo_mut();
            for i in 0..len {
                let (a, b) = input(pos + i);
                l[i] = a;
                r[i] = b;
            }
        }
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        p.process(&mut ctx);
        out_l.extend_from_slice(buf.channel(0));
        out_r.extend_from_slice(buf.channel(1));
        pos += len;
        tr.position_samples += len as i64;
        tr.position_beats += len as f64 / SR * tr.bpm / 60.0;
    }
    (out_l, out_r)
}

/// Render an instrument: `notes` are `(frame, key, velocity, on)`.
pub fn render_notes(p: &mut dyn Processor, notes: &[(usize, u8, f32, bool)], n: usize) -> (Vec<f32>, Vec<f32>) {
    p.prepare(SR, BLOCK);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let mut out_l = Vec::with_capacity(n);
    let mut out_r = Vec::with_capacity(n);
    let tr = Transport { playing: true, ..Transport::default() };
    let mut events = Vec::new();
    let mut pos = 0;
    while pos < n {
        let len = BLOCK.min(n - pos);
        buf.set_frames(len);
        buf.clear();
        events.clear();
        for &(f, key, vel, on) in notes {
            if f >= pos && f < pos + len {
                let kind = if on {
                    EventKind::NoteOn { channel: 0, key, velocity: vel, note_id: key as i32 }
                } else {
                    EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: key as i32 }
                };
                events.push(Event { offset: (f - pos) as u32, kind });
            }
        }
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &events, transport: &tr };
        p.process(&mut ctx);
        out_l.extend_from_slice(buf.channel(0));
        out_r.extend_from_slice(buf.channel(1));
        pos += len;
    }
    (out_l, out_r)
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

pub fn sine(freq: f32, amp: f32) -> impl FnMut(usize) -> (f32, f32) {
    move |i| {
        let v = amp * (std::f64::consts::TAU * freq as f64 * i as f64 / SR).sin() as f32;
        (v, v)
    }
}

/// Largest step between contiguous output samples (left channel) of a
/// steady 100 Hz sine when parameter `id` jumps from the plain value `from`
/// to `to` by an event in the middle of a block, after everything settled
/// at `from`. `setup` holds `(id, plain)` pairs applied first. Returns
/// `(step with the change, step without)`; the second is the larger of the
/// clean outputs at `from` and at `to`, the reference for "no click".
pub fn param_change_step(make: fn() -> Box<dyn Processor>, setup: &[(u32, f32)], id: u32, from: f32, to: f32, amp: f32) -> (f32, f32) {
    let run = |start: f32, change: Option<f32>| -> f32 {
        let mut p = make();
        p.prepare(SR, BLOCK);
        let norm = |p: &dyn Processor, id: u32, plain: f32| p.info().params.iter().find(|i| i.id == id).map(|i| i.to_normalized(plain)).unwrap_or_else(|| panic!("no param {id}"));
        for &(sid, v) in setup {
            let n = norm(p.as_ref(), sid, v);
            p.set_param(sid, n);
        }
        let n = norm(p.as_ref(), id, start);
        p.set_param(id, n);
        let mut buf = AudioBuffer::new(2, BLOCK);
        let tr = Transport::default();
        let mut pos = 0usize;
        let mut out = Vec::new();
        let blocks = 375 + 40;
        for b in 0..blocks {
            buf.set_frames(BLOCK);
            {
                let (l, r) = buf.stereo_mut();
                for i in 0..BLOCK {
                    let v = amp * (std::f64::consts::TAU * 100.0 * (pos + i) as f64 / SR).sin() as f32;
                    l[i] = v;
                    r[i] = v;
                }
            }
            let ev = match change {
                Some(to) if b == 375 + 10 => Some(Event { offset: (BLOCK / 2) as u32, kind: EventKind::Param { id, value: norm(p.as_ref(), id, to) } }),
                _ => None,
            };
            let events: &[Event] = match &ev {
                Some(e) => std::slice::from_ref(e),
                None => &[],
            };
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events, transport: &tr };
            p.process(&mut ctx);
            if b >= 375 + 8 {
                out.extend_from_slice(buf.channel(0));
            }
            pos += BLOCK;
        }
        out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max)
    };
    (run(from, Some(to)), run(from, None).max(run(to, None)))
}

/// Feed a one-sample pulse of 0.5 and then silence (24 576 frames in all) in
/// blocks of `block` frames and return `meter()` afterwards. The peak meters
/// must read the same whatever the host's buffer size.
pub fn meter_after_pulse(make: fn() -> Box<dyn Processor>, block: usize) -> Vec<f32> {
    let mut p = make();
    p.prepare(SR, block);
    let mut buf = AudioBuffer::new(2, block);
    let tr = Transport::default();
    let total = 24_576usize;
    let mut pos = 0;
    while pos < total {
        buf.set_frames(block);
        buf.clear();
        if pos == 0 {
            buf.channel_mut(0)[10] = 0.5;
            buf.channel_mut(1)[10] = 0.5;
        }
        let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        p.process(&mut ctx);
        pos += block;
    }
    let mut m = vec![0.0f32; 320];
    let n = p.meter(&mut m);
    m.truncate(n);
    m
}

/// RMS level of a stereo pair in dBFS (both channels together).
pub fn level_db(l: &[f32], r: &[f32]) -> f32 {
    let p = (rms(l) as f64).powi(2) + (rms(r) as f64).powi(2);
    (10.0 * (0.5 * p).max(1e-20).log10()) as f32
}

/// A fixed, deterministic stand-in for a mix at −18 dBFS RMS: pink noise, a
/// four-on-the-floor kick at 128 BPM, a bass note, off-beat hats and a slow
/// chord. Broadband, with transients, a loud low end and real stereo — what
/// a preset meets on a bus. Used to check that presets do not jump in level.
pub fn program_material(secs: f32) -> (Vec<f32>, Vec<f32>) {
    let n = (secs * SR as f32) as usize;
    let mut seed = [0x1234_5678u32, 0x9E37_79B9];
    let mut white = |c: usize| {
        let s = &mut seed[c];
        *s ^= *s << 13;
        *s ^= *s >> 17;
        *s ^= *s << 5;
        (*s >> 8) as f32 / 8_388_608.0 - 1.0
    };
    // Paul Kellet's economical pink filter.
    let mut pink = [[0.0f32; 3]; 2];
    let beat = 60.0 / 128.0;
    let mut out = [Vec::with_capacity(n), Vec::with_capacity(n)];
    let mut last_white = [0.0f32; 2];
    for i in 0..n {
        let t = i as f32 / SR as f32;
        let in_beat = t % beat;
        let kick = 0.8 * (-in_beat / 0.11).exp() * (std::f32::consts::TAU * (50.0 * in_beat + 70.0 * (1.0 - (-in_beat / 0.03).exp()) * 0.03)).sin();
        let eighth = beat / 2.0;
        let off = (t + eighth) % beat;
        let bass_gate = if (t % eighth) < eighth * 0.8 { 1.0 } else { 0.0 };
        let bass = 0.22 * bass_gate * ((std::f32::consts::TAU * 55.0 * t).sin() + 0.4 * (std::f32::consts::TAU * 110.0 * t).sin());
        let chord = 0.05 * [261.6f32, 329.6, 392.0].iter().map(|f| (std::f32::consts::TAU * f * t).sin()).sum::<f32>() * (0.7 + 0.3 * (std::f32::consts::TAU * 0.25 * t).sin());
        for c in 0..2 {
            let w = white(c);
            let p = &mut pink[c];
            p[0] = 0.99765 * p[0] + w * 0.099_046;
            p[1] = 0.963 * p[1] + w * 0.296_516_4;
            p[2] = 0.57 * p[2] + w * 1.052_691_3;
            let pn = 0.12 * (p[0] + p[1] + p[2] + w * 0.1848);
            let hp = w - last_white[c];
            last_white[c] = w;
            let pan = if c == 0 { 0.8 } else { 1.2 };
            let hat = 0.07 * pan * hp * (-(off.min(0.05)) / 0.012).exp() * if off < 0.05 { 1.0 } else { 0.0 };
            out[c].push(pn + kick + bass + chord * if c == 0 { 1.0 } else { 0.8 } + hat);
        }
    }
    let gain = 10f32.powf(-18.0 / 20.0) / (0.5 * (rms(&out[0]).powi(2) + rms(&out[1]).powi(2))).sqrt();
    for ch in &mut out {
        for v in ch.iter_mut() {
            *v *= gain;
        }
    }
    let [l, r] = out;
    (l, r)
}

/// Which part of a mix a preset is for. A hi-hat EQ that removes everything
/// below 300 Hz is level-matched on hats, not on a whole mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The whole mix.
    Mix,
    /// Kick and sub (below ~200 Hz).
    Low,
    /// Bass instruments (40 Hz – 3 kHz).
    Bass,
    /// Vocals, leads, pads, keys, guitars (120 Hz – 8 kHz).
    Mid,
    /// Hi-hats, cymbals, overheads (above ~4 kHz).
    High,
}

/// The source a preset's tags name.
pub fn source_for_tags(tags: &[String]) -> Source {
    let has = |words: &[&str]| tags.iter().any(|t| words.contains(&t.as_str()));
    if has(&["kick", "sub", "808"]) {
        Source::Low
    } else if has(&["bass"]) {
        Source::Bass
    } else if has(&["hi-hat", "hats", "cymbals", "overheads"]) {
        Source::High
    } else if has(&["vocal", "vocals", "voice", "rap", "synth", "pad", "pluck", "lead", "guitar", "keys", "piano", "strings", "snare"]) {
        Source::Mid
    } else {
        Source::Mix
    }
}

/// [`program_material`] limited to the band of `source`, back at −18 dBFS RMS.
pub fn program_for(source: Source, secs: f32) -> (Vec<f32>, Vec<f32>) {
    use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
    let (mut l, mut r) = program_material(secs);
    let stages: Vec<(SvfMode, f32)> = match source {
        Source::Mix => return (l, r),
        Source::Low => vec![(SvfMode::Lowpass, 200.0), (SvfMode::Lowpass, 200.0)],
        Source::Bass => vec![(SvfMode::Highpass, 40.0), (SvfMode::Lowpass, 3_000.0)],
        Source::Mid => vec![(SvfMode::Highpass, 120.0), (SvfMode::Lowpass, 8_000.0)],
        Source::High => vec![(SvfMode::Highpass, 4_000.0), (SvfMode::Highpass, 4_000.0)],
    };
    for (mode, f) in stages {
        let c = SvfCoeffs::new(mode, f, 0.707, 0.0, SR);
        for ch in [&mut l, &mut r] {
            let mut st = SvfState::default();
            for v in ch.iter_mut() {
                *v = st.process(&c, *v);
            }
        }
    }
    let gain = 10f32.powf(-18.0 / 20.0) / (0.5 * (rms(&l).powi(2) + rms(&r).powi(2))).sqrt();
    for v in l.iter_mut().chain(r.iter_mut()) {
        *v *= gain;
    }
    (l, r)
}

/// Run `p` (prepared and with its preset applied) over the program material
/// of `source` and report `(level change in dB over the last 2 s, peak after
/// the first 2 s, all finite)`.
pub fn level_vs_bypass(p: &mut dyn Processor, source: Source) -> (f32, f32, bool) {
    let secs = 6.0;
    let (il, ir) = program_for(source, secs);
    let tr = Transport { playing: true, bpm: 128.0, ..Transport::default() };
    let (ol, or) = process_stereo_prepared(p, |i| (il[i], ir[i]), il.len(), &tr);
    let from = il.len() - 2 * SR as usize;
    let delta = level_db(&ol[from..], &or[from..]) - level_db(&il[from..], &ir[from..]);
    let finite = ol.iter().chain(&or).all(|v| v.is_finite());
    // The first 2 s let the detectors and smoothers settle; the peak is the
    // one of the steady state.
    let warm = 2 * SR as usize;
    (delta, peak(&ol[warm..]).max(peak(&or[warm..])), finite)
}

/// The parameter that trims the output level of each mixing / mastering
/// device and time effect, for [`level_match`].
pub fn trim_param(plugin_id: &str) -> Option<u32> {
    match plugin_id {
        "fx.reverb" => Some(18),
        "fx.delay" => Some(14),
        "fx.eq" => Some(96),
        "fx.compressor" => Some(16),
        "fx.buscomp" => Some(11),
        "fx.mbdyn" => Some(9),
        _ => None,
    }
}

/// Preset authoring: bring `preset` to the level of bypass on the part of the
/// mix its tags name (and keep its steady peak under −0.3 dBFS) by setting
/// its output trim. A limiter has no trim: its input gain comes down until
/// loading it adds at most 2.5 dB. Returns the level change that remains.
pub fn level_match(preset: &mut crate::presets::Preset) -> f32 {
    let source = source_for_tags(&preset.tags);
    let measure = |preset: &crate::presets::Preset| {
        let mut p = crate::create(&preset.plugin).unwrap_or_else(|| panic!("unknown plugin {}", preset.plugin));
        p.prepare(SR, BLOCK);
        preset.apply(p.as_mut());
        level_vs_bypass(p.as_mut(), source)
    };
    let info_of = |plugin: &str, id: u32| crate::create(plugin).unwrap().info().params.iter().find(|i| i.id == id).cloned().unwrap();
    if preset.plugin == "fx.limiter" {
        let gain = info_of("fx.limiter", 0);
        for _ in 0..4 {
            let (d, ..) = measure(preset);
            if d <= 2.5 {
                break;
            }
            let g = gain.to_plain(preset.params["0"]);
            preset.params.insert("0".into(), gain.to_normalized((g - (d - 2.5)).max(0.0)));
        }
        return measure(preset).0;
    }
    let Some(id) = trim_param(&preset.plugin) else {
        return measure(preset).0;
    };
    let info = info_of(&preset.plugin, id);
    let key = id.to_string();
    let trim = |preset: &mut crate::presets::Preset| {
        for _ in 0..4 {
            let (delta, pk, _) = measure(preset);
            // Undo the level change, but never into a peak over the ceiling.
            let ceiling = 10f32.powf(-0.3 / 20.0);
            let headroom = 20.0 * (ceiling / pk.max(1e-6)).log10();
            let want = (-delta).min(headroom);
            if want.abs() < 0.05 {
                break;
            }
            let cur = info.to_plain(*preset.params.get(&key).unwrap_or(&info.default_normalized()));
            preset.params.insert(key.clone(), info.to_normalized((cur + want).clamp(info.min, info.max)));
        }
    };
    trim(preset);
    measure(preset).0
}
