//! `fx.tape` — a tape machine: Jiles–Atherton magnetic hysteresis solved
//! with second-order Runge–Kutta at 2× or 4× (Chowdhury, DAFx-19 "Real-time
//! physical modelling for analog tape machines"), then the playback side:
//! head bump (low resonance and the dip above it, tuned by tape speed),
//! spacing loss (HF roll-off, `e^{−2π f d / v}` approximated by a
//! critically damped low-pass at its −3 dB point), wow & flutter
//! (a modulated delay), stereo width, output and mix.
//!
//! Controls → model: Drive sets the anhysteretic shape `a = Ms / (0.01 +
//! 6·drive)`, Saturation lowers `Ms` (the tape saturates earlier), Bias sets
//! the reversibility `c` (under-biased tape = wide loop, more hysteresis
//! distortion). The small-signal gain of the loop is measured on every
//! change by a 192-step simulation and undone, so Drive changes the
//! character more than the level.
//!
//! Meter layout: `[input peak, output peak, magnetisation peak / Ms]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{Svf, SvfCoeffs, SvfMode};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.tape";

pub mod p {
    pub const INPUT: u32 = 0;
    pub const DRIVE: u32 = 1;
    pub const SATURATION: u32 = 2;
    pub const BIAS: u32 = 3;
    pub const SPEED: u32 = 4;
    pub const HF_LOSS: u32 = 5;
    pub const HEAD_BUMP: u32 = 6;
    pub const WOW: u32 = 7;
    pub const FLUTTER: u32 = 8;
    pub const WIDTH: u32 = 9;
    pub const OUTPUT: u32 = 10;
    pub const MIX: u32 = 11;
    pub const OVERSAMPLING: u32 = 12;
}

pub const SPEEDS: &[&str] = &["3.75 ips", "7.5 ips", "15 ips", "30 ips"];
/// Tape speed in inches per second for each `SPEEDS` entry.
pub const SPEED_IPS: [f32; 4] = [3.75, 7.5, 15.0, 30.0];
const OS: &[&str] = &["2x", "4x"];

/// Longest wow / flutter excursions (ms of delay at 100 %).
const WOW_MS: f32 = 2.5;
const FLUTTER_MS: f32 = 0.08;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::INPUT, "Input", -24.0, 24.0, 0.0).unit("dB").short("In"),
        ParamInfo::new(p::DRIVE, "Drive", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::SATURATION, "Saturation", 0.0, 100.0, 50.0).unit("%").short("Sat"),
        ParamInfo::new(p::BIAS, "Bias", 0.0, 100.0, 50.0).unit("%"),
        ParamInfo::new(p::SPEED, "Speed", 0.0, 3.0, 2.0).choices(SPEEDS),
        ParamInfo::new(p::HF_LOSS, "HF Loss", 0.0, 100.0, 30.0).unit("%").short("Loss"),
        ParamInfo::new(p::HEAD_BUMP, "Head Bump", 0.0, 100.0, 50.0).unit("%").short("Bump"),
        ParamInfo::new(p::WOW, "Wow", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::FLUTTER, "Flutter", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::WIDTH, "Width", 0.0, 200.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 24.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OVERSAMPLING, "Oversampling", 0.0, 1.0, 0.0).choices(OS).short("OS"),
    ]
}

// ───────────────────────────── Hysteresis ─────────────────────────────

/// Jiles–Atherton parameters (normalised units, `H` ≈ the audio signal).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ja {
    /// Saturation magnetisation.
    pub ms: f64,
    /// Anhysteretic shape.
    pub a: f64,
    /// Mean-field coupling.
    pub alpha: f64,
    /// Coercivity (loop width).
    pub k: f64,
    /// Reversibility.
    pub c: f64,
}

impl Ja {
    /// From the Drive, Saturation and Bias controls, each 0..1.
    pub fn from_controls(drive: f64, sat: f64, bias: f64) -> Self {
        let ms = 0.5 + 1.5 * (1.0 - sat.clamp(0.0, 1.0));
        Ja { ms, a: ms / (0.01 + 6.0 * drive.clamp(0.0, 1.0)), alpha: 1.6e-3, k: 0.478_75, c: 0.05 + 0.9 * bias.clamp(0.0, 1.0).sqrt() }
    }
}

/// Langevin function `L(x) = coth x − 1/x` and its derivative
/// `L'(x) = 1/x² − 1/sinh² x`, with one `exp`.
#[inline]
pub fn langevin(x: f64) -> (f64, f64) {
    let ax = x.abs();
    if ax < 1e-3 {
        return (x / 3.0 - x * x * x / 45.0, 1.0 / 3.0 - x * x / 15.0);
    }
    let e = (-2.0 * ax).exp();
    let coth = (1.0 + e) / (1.0 - e);
    let csch2 = 4.0 * e / ((1.0 - e) * (1.0 - e));
    let l = coth - 1.0 / ax;
    (l.copysign(x), 1.0 / (ax * ax) - csch2)
}

/// One channel of the hysteresis solver.
#[derive(Clone, Copy, Debug)]
pub struct Hysteresis {
    pub ja: Ja,
    m: f64,
    h1: f64,
    dh1: f64,
}

impl Hysteresis {
    pub fn new(ja: Ja) -> Self {
        Self { ja, m: 0.0, h1: 0.0, dh1: 0.0 }
    }

    pub fn reset(&mut self) {
        self.m = 0.0;
        self.h1 = 0.0;
        self.dh1 = 0.0;
    }

    /// `dM` for a field step `dh` around `(m, h)` (the JA ODE times `dt`,
    /// so the sample rate cancels): Chowdhury 2019, eq. (8)–(10).
    #[inline]
    fn dm(&self, m: f64, h: f64, dh: f64) -> f64 {
        let Ja { ms, a, alpha, k, c } = self.ja;
        let q = (h + alpha * m) / a;
        let (l, lp) = langevin(q);
        let diff = ms * l - m;
        let delta = if dh > 0.0 {
            1.0
        } else if dh < 0.0 {
            -1.0
        } else {
            return 0.0;
        };
        let delta_m = if (delta > 0.0) == (diff > 0.0) { 1.0 } else { 0.0 };
        let den = (1.0 - c) * delta * k - alpha * diff;
        let irr = if den.abs() > 1e-9 { (1.0 - c) * delta_m * diff / den } else { 0.0 };
        let rev = c * ms / a * lp;
        dh * (irr + rev) / (1.0 - alpha * rev)
    }

    /// Advance to field `h`; returns the magnetisation. Large field steps
    /// (hot, bright input) are split into up to 4 sub-steps along the
    /// straight line from the previous field: RK2 on a big step overshoots
    /// the loop and turns into noise.
    #[inline]
    pub fn process(&mut self, h: f64) -> f64 {
        let total = h - self.h1;
        let n = (total.abs() / MAX_STEP).ceil().clamp(1.0, 4.0);
        let dh = total / n;
        for _ in 0..n as usize {
            let h0 = self.h1;
            let h = h0 + dh;
            // RK2 (midpoint) on M(H) along the segment from h0 to h.
            let k1 = self.dm(self.m, h0, 0.5 * (dh + self.dh1));
            let k2 = self.dm(self.m + 0.5 * k1, 0.5 * (h + h0), dh);
            self.m += k2;
            self.h1 = h;
            self.dh1 = dh;
        }
        // A runaway (it should not happen with the clamped c) resets.
        if !self.m.is_finite() || self.m.abs() > 4.0 * self.ja.ms {
            self.m = 0.0;
        }
        if self.m.abs() < 1e-30 {
            self.m = 0.0;
        }
        self.m
    }
}

/// Largest field step one RK2 step takes.
const MAX_STEP: f64 = 0.6;

/// Field per unit of signal: 0 dBFS drives the tape well into saturation,
/// −20 dBFS stays near the linear part of the loop.
pub const H_SCALE: f64 = 4.0;

/// Gain that brings a −20 dBFS sine back to its level through the loop
/// (signal → `H_SCALE · x` → M → `makeup · M`): three cycles of 64 steps
/// on a fresh solver, peak of the last one.
pub fn makeup(ja: Ja) -> f64 {
    let mut h = Hysteresis::new(ja);
    let amp = 0.1;
    let mut peak: f64 = 0.0;
    for i in 0..192 {
        let m = h.process(H_SCALE * amp * (std::f64::consts::TAU * i as f64 / 64.0).sin());
        if i >= 128 {
            peak = peak.max(m.abs());
        }
    }
    if peak > 1e-9 { (amp / peak).clamp(0.05, 50.0) } else { 1.0 }
}

/// The loop the UI draws: `(H, M / Ms)` over one cycle of a sine of
/// amplitude `amp` after a settling cycle. Allocates (UI only).
pub fn loop_points(ja: Ja, amp: f64, n: usize) -> Vec<(f32, f32)> {
    let mut h = Hysteresis::new(ja);
    let mut out = Vec::with_capacity(n + 1);
    for i in 0..=(2 * n) {
        let x = amp * (std::f64::consts::TAU * i as f64 / n as f64).sin();
        let m = h.process(x);
        if i >= n {
            out.push((x as f32, (m / ja.ms) as f32));
        }
    }
    out
}

// ───────────────────────────── Playback side ─────────────────────────────

/// Head bump frequency (Hz) at a tape speed (ips).
pub fn bump_freq(ips: f32) -> f32 {
    3.4 * ips
}

/// −3 dB frequency of the spacing loss for `hf_loss` 0..1 at `ips`:
/// `e^{−2π f d / v} = 1/√2` → `f = ln√2 · v / (2π d)`, with the head-to-tape
/// spacing `d` = 0.2…3.2 µm.
pub fn loss_freq(ips: f32, hf_loss: f32) -> f32 {
    let v = ips * 0.0254;
    let d = (0.2 + 3.0 * hf_loss.clamp(0.0, 1.0)) * 1e-6;
    std::f32::consts::LN_2 * 0.5 * v / (std::f32::consts::TAU * d)
}

/// The playback filters: bump bell, the dip above it, spacing low-pass.
pub fn playback_coeffs(ips: f32, hf_loss: f32, bump: f32, sample_rate: f64) -> [SvfCoeffs; 3] {
    let fb = bump_freq(ips);
    let gain = 6.0 * bump.clamp(0.0, 1.0);
    let lf = loss_freq(ips, hf_loss);
    let lp = if lf < 0.45 * sample_rate as f32 { SvfCoeffs::new(SvfMode::Lowpass, lf, 0.5, 0.0, sample_rate) } else { SvfCoeffs::passthrough() };
    [SvfCoeffs::new(SvfMode::Bell, fb, 1.2, gain, sample_rate), SvfCoeffs::new(SvfMode::Bell, 2.3 * fb, 1.5, -0.4 * gain, sample_rate), lp]
}

/// Magnitude (dB) of the playback filters at `freq` (UI curve).
pub fn playback_db(ips: f32, hf_loss: f32, bump: f32, freq: f64, sample_rate: f64) -> f32 {
    playback_coeffs(ips, hf_loss, bump, sample_rate).iter().map(|c| c.magnitude_db(freq, sample_rate)).sum::<f64>() as f32
}

// ───────────────────────────── Plugin ─────────────────────────────

#[derive(Clone, Copy)]
struct Channel {
    os: Oversampler,
    hyst: Hysteresis,
}

pub struct Tape {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    ch: [Channel; 2],
    filters: [[Svf; 3]; 2],
    flutter_line: [DelayLine; 2],
    dry_delay: [DelayLine; 2],
    input: Smoother,
    makeup: Smoother,
    wow: Smoother,
    flutter: Smoother,
    width: Smoother,
    wet: Smoother,
    dry: Smoother,
    /// Wow, flutter partials and drift phases (cycles).
    phase: [f32; 3],
    drift: f32,
    drift_target: f32,
    seed: u32,
    factor: usize,
    latency: usize,
    peaks: [f32; 3],
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Tape::new())
}

impl Default for Tape {
    fn default() -> Self {
        Self::new()
    }
}

impl Tape {
    pub fn new() -> Self {
        let params = params();
        let ja = Ja::from_controls(0.5, 0.5, 0.5);
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Tape",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["saturation", "tape", "hysteresis", "wow", "flutter", "vintage"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            ch: [Channel { os: Oversampler::new(2), hyst: Hysteresis::new(ja) }; 2],
            filters: [[Svf::default(); 3]; 2],
            flutter_line: [DelayLine::new(16), DelayLine::new(16)],
            dry_delay: [DelayLine::new(16), DelayLine::new(16)],
            input: Smoother::new(1.0),
            makeup: Smoother::new(1.0),
            wow: Smoother::new(0.0),
            flutter: Smoother::new(0.0),
            width: Smoother::new(1.0),
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            phase: [0.0, 0.31, 0.77],
            drift: 0.0,
            drift_target: 0.0,
            seed: 0x1234_5678,
            factor: 0,
            latency: 0,
            peaks: [0.0; 3],
            first: true,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let factor = if self.params.choice(p::OVERSAMPLING) == 0 { 2 } else { 4 };
        if factor != self.factor {
            self.factor = factor;
            for c in &mut self.ch {
                c.os.set_factor(factor);
                c.hyst.reset();
            }
            self.latency = self.ch[0].os.latency().round() as usize;
        }
        let ja = Ja::from_controls(self.params.plain(p::DRIVE) as f64 / 100.0, self.params.plain(p::SATURATION) as f64 / 100.0, self.params.plain(p::BIAS) as f64 / 100.0);
        if ja != self.ch[0].hyst.ja || self.first {
            for c in &mut self.ch {
                c.hyst.ja = ja;
            }
            self.makeup.set_target(makeup(ja) as f32);
        }
        let ips = SPEED_IPS[self.params.choice(p::SPEED).min(3)];
        let coeffs = playback_coeffs(ips, self.params.plain(p::HF_LOSS) / 100.0, self.params.plain(p::HEAD_BUMP) / 100.0, self.sr);
        for f in &mut self.filters {
            for (svf, c) in f.iter_mut().zip(coeffs) {
                svf.coeffs = c;
            }
        }
        self.input.set_target(db_to_gain(self.params.plain(p::INPUT)));
        self.wow.set_target(self.params.plain(p::WOW) / 100.0 * WOW_MS * 0.001 * self.sr as f32);
        self.flutter.set_target(self.params.plain(p::FLUTTER) / 100.0 * FLUTTER_MS * 0.001 * self.sr as f32);
        self.width.set_target(self.params.plain(p::WIDTH) / 100.0);
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        if self.first {
            for s in [&mut self.input, &mut self.makeup, &mut self.wow, &mut self.flutter, &mut self.width, &mut self.wet, &mut self.dry] {
                s.snap_to_target();
            }
            self.first = false;
        }
    }
}

impl Segmented for Tape {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let factor = self.factor;
        let lat = self.latency;
        let sr = self.sr as f32;
        let decay = (-1.0 / (0.3 * sr)).exp();
        // Wow ≈ 0.6 Hz with a slow random drift, flutter at 12 and 19.7 Hz.
        let inc = [0.6 / sr, 12.0 / sr, 19.7 / sr];
        let drift_coeff = 1.0 - (-1.0 / (0.5 * sr)).exp();
        let inv_ms = (1.0 / self.ch[0].hyst.ja.ms) as f32;
        let Self { ch, filters, flutter_line, dry_delay, input, makeup, wow, flutter, width, wet, dry, phase, drift, drift_target, seed, peaks, .. } = self;
        let mut pk = *peaks;
        let mut ms_peak = 0.0f32;
        let tau = std::f32::consts::TAU;
        for_frames(ctx.buffer, start, end, |i, l, r| {
            let (l, r) = (sanitize(l).clamp(-64.0, 64.0), sanitize(r).clamp(-64.0, 64.0));
            // Transport modulation, shared by both channels.
            if i % 2048 == 0 {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *drift_target = (*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            }
            *drift += (*drift_target - *drift) * drift_coeff;
            for (ph, d) in phase.iter_mut().zip(inc) {
                *ph += d;
                if *ph >= 1.0 {
                    *ph -= 1.0;
                }
            }
            let w = wow.next() * (1.0 + 0.8 * (tau * phase[0]).sin() + 0.2 * *drift);
            let f = flutter.next() * (1.0 + 0.6 * (tau * phase[1]).sin() + 0.4 * (tau * phase[2]).sin());
            let delay = (w + f).max(0.0);
            let (gin, mk, wd, gw, gd) = (input.next(), makeup.next(), width.next(), wet.next(), dry.next());
            pk[0] = (pk[0] * decay).max(l.abs().max(r.abs()));
            let mut out = [l, r];
            for (c, x) in out.iter_mut().enumerate() {
                let chn = &mut ch[c];
                let mut up = [0.0f32; 4];
                chn.os.upsample(*x * gin, &mut up);
                for s in up.iter_mut().take(factor) {
                    let m = chn.hyst.process(*s as f64 * H_SCALE) as f32;
                    ms_peak = ms_peak.max(m.abs() * inv_ms);
                    *s = m * mk;
                }
                let mut y = chn.os.downsample(&up);
                for svf in &mut filters[c] {
                    y = svf.process(y);
                }
                let line = &mut flutter_line[c];
                line.push(y);
                let y = if delay >= 1.0 { line.read_cubic(delay) } else { line.read_linear(delay) };
                dry_delay[c].push(*x);
                *x = dry_delay[c].read(lat) * gd + y * gw;
            }
            let mid = 0.5 * (out[0] + out[1]);
            let side = 0.5 * (out[0] - out[1]) * wd;
            let (l, r) = (sanitize(mid + side), sanitize(mid - side));
            pk[1] = (pk[1] * decay).max(l.abs().max(r.abs()));
            (l, r)
        });
        pk[2] = (pk[2] * decay.powi((end - start) as i32)).max(ms_peak);
        *peaks = pk.map(|v| if v.is_finite() { v } else { 0.0 });
    }
}

impl Processor for Tape {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.input, &mut self.makeup, &mut self.wow, &mut self.flutter, &mut self.width, &mut self.wet, &mut self.dry] {
            s.set_time(30.0, sample_rate);
        }
        let max = ((WOW_MS * 2.0 + FLUTTER_MS * 2.0) * 0.001 * sample_rate as f32) as usize + 8;
        self.flutter_line = [DelayLine::new(max), DelayLine::new(max)];
        self.dry_delay = [DelayLine::new(64), DelayLine::new(64)];
        self.factor = 0;
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for c in &mut self.ch {
            c.os.reset();
            c.hyst.reset();
        }
        for f in self.filters.iter_mut().flatten() {
            f.reset();
        }
        for d in self.flutter_line.iter_mut().chain(self.dry_delay.iter_mut()) {
            d.clear();
        }
        self.peaks = [0.0; 3];
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

    fn latency(&self) -> u32 {
        self.latency as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &self.peaks)
    }
}

#[cfg(test)]
mod tests {
    use super::super::shapes::{alias_db, assert_cpu, power};
    use super::*;
    use crate::test_util::{SR, process_stereo, rms, set_plain, sine};

    #[test]
    fn langevin_matches_its_definition() {
        for x in [-30.0, -3.0, -0.5, -0.01, 0.002, 0.2, 1.0, 4.0, 25.0] {
            let (l, lp) = langevin(x);
            let want = 1.0 / x.tanh() - 1.0 / x;
            assert!((l - want).abs() < 1e-9, "L({x}) = {l} vs {want}");
            let h = 1e-5;
            let d = (langevin(x + h).0 - langevin(x - h).0) / (2.0 * h);
            assert!((lp - d).abs() < 1e-5, "L'({x}) = {lp} vs {d}");
        }
    }

    /// The solver traces a real loop: going up and coming down the same
    /// field gives different magnetisations (remanence), and a wider loop
    /// for less bias.
    #[test]
    fn hysteresis_has_memory() {
        let area = |bias: f64| {
            let pts = loop_points(Ja::from_controls(0.5, 0.5, bias), 1.0, 256);
            // Shoelace area of the closed (H, M) curve.
            let mut a = 0.0f64;
            for w in pts.windows(2) {
                a += (w[0].0 * w[1].1 - w[1].0 * w[0].1) as f64;
            }
            (0.5 * a).abs()
        };
        let (under, over) = (area(0.1), area(0.9));
        assert!(under > 0.02, "loop area {under}");
        assert!(under > 1.5 * over, "under-biased {under} vs over-biased {over}");
        // Remanence: M at H = 0 on the way down is positive.
        let pts = loop_points(Ja::from_controls(0.5, 0.5, 0.2), 1.0, 256);
        let down = pts.iter().skip(64).find(|p| p.0 <= 0.0).unwrap();
        assert!(down.1 > 0.01, "remanence {}", down.1);
    }

    #[test]
    fn unity_ish_at_low_level_and_saturates_when_hot() {
        let mut t = Tape::new();
        set_plain(&mut t, p::HEAD_BUMP, 0.0);
        set_plain(&mut t, p::HF_LOSS, 0.0);
        let (l, _) = process_stereo(&mut t, sine(1000.0, 0.1), 48_000);
        let g = 20.0 * (rms(&l[24_000..]) / (0.1 / std::f32::consts::SQRT_2)).log10();
        assert!(g.abs() < 1.5, "gain at −20 dBFS {g} dB");
        // +18 dB in: the output grows much less than 18 dB and gains odd harmonics.
        set_plain(&mut t, p::INPUT, 18.0);
        let (l2, _) = process_stereo(&mut t, sine(1000.0, 0.1), 48_000);
        let x = &l2[24_000..];
        let g2 = 20.0 * (rms(x) / rms(&l[24_000..])).log10();
        assert!(g2 < 14.0, "+18 dB in gave +{g2} dB out");
        let h3 = 10.0 * (power(x, 3000.0, SR) / power(x, 1000.0, SR)).log10();
        assert!(h3 > -35.0, "3rd harmonic {h3} dB");
    }

    #[test]
    fn head_bump_and_loss_follow_speed() {
        let sr = 48_000.0;
        // The bump sits higher at faster speeds; the loss corner too.
        assert!(bump_freq(30.0) > bump_freq(7.5));
        assert!(loss_freq(30.0, 0.5) > 3.0 * loss_freq(7.5, 0.5));
        let at = |ips: f32, f: f64| playback_db(ips, 0.5, 1.0, f, sr);
        assert!(at(15.0, bump_freq(15.0) as f64) > 4.0);
        assert!(at(15.0, 1000.0).abs() < 1.0);
        assert!(at(3.75, 10_000.0) < -12.0);
        assert!(at(30.0, 10_000.0) > -6.0);
    }

    #[test]
    fn wow_and_flutter_modulate_pitch() {
        let mut t = Tape::new();
        set_plain(&mut t, p::HEAD_BUMP, 0.0);
        set_plain(&mut t, p::HF_LOSS, 0.0);
        set_plain(&mut t, p::INPUT, -12.0);
        let (clean, _) = process_stereo(&mut t, sine(2000.0, 0.1), 96_000);
        set_plain(&mut t, p::WOW, 100.0);
        set_plain(&mut t, p::FLUTTER, 100.0);
        let (wobbly, _) = process_stereo(&mut t, sine(2000.0, 0.1), 96_000);
        // Frequency modulation spreads the tone: less energy left in its bin.
        let p_clean = power(&clean[48_000..], 2000.0, SR);
        let p_wob = power(&wobbly[48_000..], 2000.0, SR);
        assert!(p_wob < 0.7 * p_clean, "clean {p_clean} wobbly {p_wob}");
        assert!(wobbly.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn width_zero_is_mono() {
        let mut t = Tape::new();
        set_plain(&mut t, p::WIDTH, 0.0);
        let (l, r) = process_stereo(&mut t, |i| ((i as f32 * 0.01).sin() * 0.3, (i as f32 * 0.013).cos() * 0.3), 9600);
        for (a, b) in l.iter().zip(&r).skip(4800) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn low_aliasing_when_driven() {
        let mut t = Tape::new();
        set_plain(&mut t, p::INPUT, 12.0);
        set_plain(&mut t, p::HF_LOSS, 0.0);
        set_plain(&mut t, p::HEAD_BUMP, 0.0);
        let (l, _) = process_stereo(&mut t, sine(4987.0, 0.4), 3 << 14);
        let a2 = alias_db(&l[1 << 15..], 4987.0, SR);
        set_plain(&mut t, p::OVERSAMPLING, 1.0);
        let (l, _) = process_stereo(&mut t, sine(4987.0, 0.4), 3 << 14);
        let a4 = alias_db(&l[1 << 15..], 4987.0, SR);
        assert!(a2 < -35.0, "2x aliases {a2:.1} dB");
        assert!(a4 < a2, "4x {a4:.1} dB vs 2x {a2:.1} dB");
    }

    #[test]
    fn silence_stays_silent_and_finite_on_garbage() {
        let mut t = Tape::new();
        set_plain(&mut t, p::WOW, 50.0);
        let (l, _) = process_stereo(&mut t, |_| (0.0, 0.0), 48_000);
        assert!(l.iter().all(|v| *v == 0.0 || v.abs() < 1e-20));
        let (l, _) = process_stereo(&mut t, |i| if i % 7 == 0 { (1e9, -1e9) } else { (f32::NAN, 0.0) }, 4800);
        assert!(l.iter().all(|v| v.is_finite()));
        let (l, _) = process_stereo(&mut t, sine(440.0, 0.5), 4800);
        assert!(l.iter().all(|v| v.is_finite()) && rms(&l[2400..]) > 0.05);
    }

    #[test]
    fn cpu_budget() {
        let mut t = Tape::new();
        set_plain(&mut t, p::OVERSAMPLING, 1.0);
        set_plain(&mut t, p::WOW, 50.0);
        set_plain(&mut t, p::FLUTTER, 50.0);
        assert_cpu(&mut t, 6.0);
    }
}
