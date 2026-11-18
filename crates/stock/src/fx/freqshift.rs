//! `fx.freqshift` — frequency shifter. A Hilbert transform (two chains of
//! all-pass sections that stay 90° apart over almost the whole audio band)
//! turns the input into a quadrature pair, which a quadrature oscillator
//! rotates: single-sideband modulation moves every partial by the same
//! number of Hz, so harmonic sounds turn inharmonic, and a slow shift with
//! feedback gives the barber-pole sweep. Ring mode multiplies by the carrier
//! instead (both sidebands). The shift can be modulated by an LFO and split
//! into opposite shifts per side. Zero latency.
//!
//! Meter layout: `[shift left Hz, shift right Hz, input peak, output peak, lfo (−1..1)]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::lfo::{Lfo, LfoShape};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.freqshift";

pub mod p {
    pub const MODE: u32 = 0;
    pub const SHIFT: u32 = 1;
    pub const FINE: u32 = 2;
    pub const LFO_SHAPE: u32 = 3;
    pub const LFO_RATE: u32 = 4;
    pub const LFO_DEPTH: u32 = 5;
    pub const FEEDBACK: u32 = 6;
    pub const SPREAD: u32 = 7;
    pub const MIX: u32 = 8;
    pub const OUTPUT: u32 = 9;
}

const MODES: &[&str] = &["Shift", "Ring"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 1.0, 0.0).choices(MODES),
        ParamInfo::new(p::SHIFT, "Shift", -2000.0, 2000.0, 0.0).unit("Hz"),
        ParamInfo::new(p::FINE, "Fine", -20.0, 20.0, 0.0).unit("Hz"),
        ParamInfo::new(p::LFO_SHAPE, "LFO Shape", 0.0, 6.0, 0.0).choices(LfoShape::NAMES).short("Shape"),
        ParamInfo::new(p::LFO_RATE, "LFO Rate", 0.02, 20.0, 1.0).unit("Hz").log().short("Rate"),
        ParamInfo::new(p::LFO_DEPTH, "LFO Depth", 0.0, 1000.0, 0.0).unit("Hz").short("Depth"),
        ParamInfo::new(p::FEEDBACK, "Feedback", 0.0, 90.0, 0.0).unit("%").short("FB"),
        ParamInfo::new(p::SPREAD, "Stereo Spread", 0.0, 100.0, 0.0).unit("%").short("Spread"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
    ]
}

/// All-pass pole coefficients (squares are used) of the two Hilbert chains
/// (O. Niemitalo's 8-section design: 90° ±0.7° from ~0.002 to 0.498 of fs).
const CHAIN_A: [f32; 4] = [0.6923878, 0.936_065_4, 0.988_229_5, 0.998_748_8];
const CHAIN_B: [f32; 4] = [0.402_192_1, 0.856_171_1, 0.972_290_95, 0.995_288_5];

/// Four second-order all-pass sections `y = a (x + y₂) − x₂`.
#[derive(Clone, Copy, Default)]
struct Chain {
    x1: [f32; 4],
    x2: [f32; 4],
    y1: [f32; 4],
    y2: [f32; 4],
}

impl Chain {
    #[inline]
    fn process(&mut self, mut x: f32, coeffs: &[f32; 4]) -> f32 {
        for k in 0..4 {
            let a = coeffs[k] * coeffs[k];
            let y = a * (x + self.y2[k]) - self.x2[k];
            self.x2[k] = self.x1[k];
            self.x1[k] = x;
            self.y2[k] = self.y1[k];
            self.y1[k] = if y.abs() < 1e-20 { 0.0 } else { y };
            x = y;
        }
        x
    }
}

/// Quadrature pair of one channel: `(in-phase, quadrature)`.
#[derive(Clone, Copy, Default)]
struct Hilbert {
    a: Chain,
    b: Chain,
    delayed: f32,
}

impl Hilbert {
    #[inline]
    fn process(&mut self, x: f32) -> (f32, f32) {
        // The second chain is fed one sample late: that is what puts the
        // outputs a quarter turn apart.
        let i = self.a.process(self.delayed, &CHAIN_A);
        let q = self.b.process(x, &CHAIN_B);
        self.delayed = x;
        (i, q)
    }
}

pub struct FreqShifter {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    hilbert: [Hilbert; 2],
    phase: [f64; 2],
    lfo: Lfo,
    fb_state: [f32; 2],
    shift: Smoother,
    depth: Smoother,
    spread: Smoother,
    feedback: Smoother,
    wet: Smoother,
    dry: Smoother,
    first: bool,
    cur: [f32; 2],
    lfo_value: f32,
    in_peak: f32,
    out_peak: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(FreqShifter::new())
}

impl Default for FreqShifter {
    fn default() -> Self {
        Self::new()
    }
}

impl FreqShifter {
    pub fn new() -> Self {
        let params = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Frequency Shifter",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["pitch", "frequency shifter", "ring modulator", "single sideband", "inharmonic", "barber pole"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            hilbert: [Hilbert::default(); 2],
            phase: [0.0; 2],
            lfo: Lfo::new(0x5eed),
            fb_state: [0.0; 2],
            shift: Smoother::new(0.0),
            depth: Smoother::new(0.0),
            spread: Smoother::new(0.0),
            feedback: Smoother::new(0.0),
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            first: true,
            cur: [0.0; 2],
            lfo_value: 0.0,
            in_peak: 0.0,
            out_peak: 0.0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        self.shift.set_target(self.params.plain(p::SHIFT) + self.params.plain(p::FINE));
        self.depth.set_target(self.params.plain(p::LFO_DEPTH));
        self.spread.set_target(self.params.plain(p::SPREAD) / 100.0);
        self.feedback.set_target(self.params.plain(p::FEEDBACK) / 100.0);
        self.lfo.shape = LfoShape::from_index(self.params.choice(p::LFO_SHAPE));
        self.lfo.set_rate(self.params.plain(p::LFO_RATE), self.sr);
        if self.first {
            for s in [&mut self.wet, &mut self.dry, &mut self.shift, &mut self.depth, &mut self.spread, &mut self.feedback] {
                s.snap_to_target();
            }
            self.first = false;
        }
    }
}

impl Segmented for FreqShifter {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let ring = self.params.choice(p::MODE) == 1;
        let inv_sr = 1.0 / self.sr;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let lfo = self.lfo.next();
            self.lfo_value = lfo;
            let f = self.shift.next() + self.depth.next() * lfo;
            let spread = self.spread.next();
            let fb = self.feedback.next();
            let freqs = [f, f * (1.0 - 2.0 * spread)];
            self.cur = freqs;
            let input = [l, r];
            self.in_peak = (self.in_peak * 0.9995).max(l.abs().max(r.abs()));
            let mut out = [0.0f32; 2];
            for c in 0..2 {
                self.phase[c] += freqs[c] as f64 * inv_sr;
                self.phase[c] -= self.phase[c].floor();
                let (sin, cos) = (std::f64::consts::TAU * self.phase[c]).sin_cos();
                let x = input[c] + fb * self.fb_state[c];
                let (i, q) = self.hilbert[c].process(x);
                let y = if ring { x * cos as f32 } else { i * cos as f32 + q * sin as f32 };
                out[c] = sanitize(y);
                self.fb_state[c] = sanitize(y.clamp(-2.0, 2.0));
            }
            let (w, d) = (self.wet.next(), self.dry.next());
            let o = (sanitize(l * d + out[0] * w), sanitize(r * d + out[1] * w));
            self.out_peak = (self.out_peak * 0.9995).max(o.0.abs().max(o.1.abs()));
            o
        });
    }
}

impl Processor for FreqShifter {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.wet, &mut self.dry, &mut self.depth, &mut self.spread, &mut self.feedback] {
            s.set_time(20.0, sample_rate);
        }
        self.shift.set_time(12.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        self.hilbert = [Hilbert::default(); 2];
        self.phase = [0.0; 2];
        self.fb_state = [0.0; 2];
        self.lfo.reset(0.0);
        self.in_peak = 0.0;
        self.out_peak = 0.0;
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        drive(self, ctx);
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.params.set(id, normalized);
    }

    fn get_param(&self, id: u32) -> f32 {
        self.params.get(id)
    }

    fn tail(&self) -> u32 {
        let fb = self.params.plain(p::FEEDBACK) / 100.0;
        if fb > 0.01 { (self.sr * 0.5) as u32 } else { 256 }
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.cur[0], self.cur[1], self.in_peak.min(4.0), self.out_peak.min(4.0), self.lfo_value])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, set_plain, sine};

    fn level_at(x: &[f32], freq: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        let n = x.len() as f64;
        for (i, &v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos();
            let a = std::f64::consts::TAU * freq as f64 * i as f64 / SR;
            re += v as f64 * w * a.cos();
            im += v as f64 * w * a.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / (n * 0.5)) as f32
    }

    #[test]
    fn shift_moves_a_sine_by_the_set_hz() {
        for (src, hz) in [(1000.0f32, 150.0f32), (1000.0, -300.0), (3000.0, 1200.0), (440.0, 40.0)] {
            let mut s = FreqShifter::new();
            set_plain(&mut s, p::SHIFT, hz);
            let (l, _) = process_stereo(&mut s, sine(src, 0.5), 24_000);
            let tail = &l[8_000..];
            let want = level_at(tail, src + hz);
            let image = level_at(tail, src - hz);
            let orig = level_at(tail, src);
            assert!(want > 0.45, "{src}+{hz}: shifted line {want}");
            assert!(image < 0.05 * want, "{src}+{hz}: opposite sideband {image} vs {want}");
            assert!(orig < 0.05 * want, "{src}+{hz}: original {orig}");
        }
    }

    #[test]
    fn ring_mode_makes_both_sidebands() {
        let mut s = FreqShifter::new();
        set_plain(&mut s, p::MODE, 1.0);
        set_plain(&mut s, p::SHIFT, 200.0);
        let (l, _) = process_stereo(&mut s, sine(1000.0, 0.5), 24_000);
        let tail = &l[8_000..];
        let (up, down, orig) = (level_at(tail, 1200.0), level_at(tail, 800.0), level_at(tail, 1000.0));
        assert!(up > 0.2 && down > 0.2, "{up} {down}");
        assert!((up - down).abs() < 0.03);
        assert!(orig < 0.02);
    }

    #[test]
    fn spread_shifts_the_sides_in_opposite_directions() {
        let mut s = FreqShifter::new();
        set_plain(&mut s, p::SHIFT, 100.0);
        set_plain(&mut s, p::SPREAD, 100.0);
        let (l, r) = process_stereo(&mut s, sine(1000.0, 0.5), 24_000);
        assert!(level_at(&l[8_000..], 1100.0) > 0.4);
        assert!(level_at(&r[8_000..], 900.0) > 0.4);
        assert!(level_at(&r[8_000..], 1100.0) < 0.03);
    }

    #[test]
    fn lfo_sweeps_the_shift() {
        let mut s = FreqShifter::new();
        set_plain(&mut s, p::LFO_DEPTH, 400.0);
        set_plain(&mut s, p::LFO_RATE, 2.0);
        let (l, _) = process_stereo(&mut s, sine(1000.0, 0.5), 48_000);
        // With no static shift the line splits around 1 kHz; energy exists
        // away from the source frequency.
        let tail = &l[8_000..];
        assert!(level_at(tail, 1000.0) < 0.3);
        assert!(peak(tail) > 0.3);
    }

    #[test]
    fn mix_zero_is_dry_and_feedback_is_bounded() {
        let mut s = FreqShifter::new();
        set_plain(&mut s, p::SHIFT, 500.0);
        set_plain(&mut s, p::MIX, 0.0);
        let (l, _) = process_stereo(&mut s, sine(440.0, 0.5), 6000);
        let (want, _) = sine(440.0, 0.5)(4000);
        assert!((l[4000] - want).abs() < 1e-4);
        let mut s = FreqShifter::new();
        set_plain(&mut s, p::SHIFT, 3.0);
        set_plain(&mut s, p::FEEDBACK, 90.0);
        let (l, r) = process_stereo(&mut s, |i| if i % 997 == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 96_000);
        assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 40.0));
    }
}
