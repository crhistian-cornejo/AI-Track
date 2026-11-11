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
