//! `fx.autopan` — auto-pan and tremolo. Nine LFO shapes, free (Hz or time) or
//! tempo-synced rate, stereo phase offset or spin, shape bending, attack
//! ramp after silence, input-driven rate modulation, plus harmonic and
//! vintage tremolo flavours. Zero latency.
//!
//! Pan mode gives each channel its own LFO and the gain
//! `(1 − a) + a·cos((v + 1)·π/4)` (`v` = LFO, `a` = Amount): never above 1,
//! so the output peak cannot exceed the input peak. At Amount 100 % that is
//! the constant-power law (−3 dB at the centre, 0 dB at the extremes; with a
//! 180° offset the two channels trade level and the summed power stays
//! constant); at Amount 0 the gain is exactly 1. Tremolo mode is `1 − Amount·(1 − (lfo + 1)/2)` per channel.
//!
//! Meter layout: `[LFO left, LFO right, phase (0..1), input level (0..1)]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::crossover::{Lr4, Lr4Coeffs};
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use aitrack_dsp::lfo::{PhaseLfo, SYNC_DIVISIONS, Waveform, division_beats, division_hz};
use aitrack_dsp::smooth::{Smoother, one_pole_coeff};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};
use std::f32::consts::FRAC_PI_4;

/// Crossover of the harmonic tremolo.
const SPLIT_HZ: f32 = 600.0;
/// Input level (linear) that counts as signal for the attack ramp: −50 dBFS.
const ATTACK_THRESHOLD: f32 = 0.003_162_277_7;
/// Quiet time before a new onset restarts the attack ramp, seconds.
const ATTACK_REARM_S: f64 = 0.1;
/// Cross-fade between signal paths, ms.
const FADE_MS: f64 = 5.0;
/// Right-channel rate offset of Spin at 100 %.
const SPIN_SPREAD: f64 = 0.1;

pub const ID: &str = "fx.autopan";

pub mod p {
    pub const MODE: u32 = 0;
    pub const WAVE: u32 = 1;
    pub const TIME_MODE: u32 = 2;
    pub const RATE: u32 = 3;
    pub const TIME: u32 = 4;
    pub const DIVISION: u32 = 5;
    pub const PHASE: u32 = 6;
    pub const STEREO_MODE: u32 = 7;
    pub const STEREO: u32 = 8;
    pub const INVERT: u32 = 9;
    pub const AMOUNT: u32 = 10;
    pub const SHAPE: u32 = 11;
    pub const ATTACK: u32 = 12;
    pub const DYN_FM: u32 = 13;
    pub const HARMONIC: u32 = 14;
    pub const VINTAGE: u32 = 15;
    pub const MIX: u32 = 16;
    pub const OUTPUT: u32 = 17;
}

const MODES: &[&str] = &["Pan", "Tremolo"];
const TIME_MODES: &[&str] = &["Hz", "Time", "Sync"];
const STEREO_MODES: &[&str] = &["Phase", "Spin"];
const OFF_ON: &[&str] = &["Off", "On"];

pub fn params() -> Vec<ParamInfo> {
    let n_div = (SYNC_DIVISIONS.len() - 1) as f32;
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 1.0, 0.0).choices(MODES),
        ParamInfo::new(p::WAVE, "Wave", 0.0, 8.0, 0.0).choices(Waveform::NAMES),
        ParamInfo::new(p::TIME_MODE, "Time Mode", 0.0, 2.0, 2.0).choices(TIME_MODES).short("Time Mode"),
        ParamInfo::new(p::RATE, "Rate", 0.01, 40.0, 1.0).unit("Hz").log(),
        ParamInfo::new(p::TIME, "Time", 100.0, 20_000.0, 1000.0).unit("ms").log(),
        ParamInfo::new(p::DIVISION, "Division", 0.0, n_div, 7.0).choices(SYNC_DIVISIONS).short("Div"),
        ParamInfo::new(p::PHASE, "Phase", 0.0, 360.0, 0.0).unit("°"),
        ParamInfo::new(p::STEREO_MODE, "Stereo Mode", 0.0, 1.0, 0.0).choices(STEREO_MODES).short("St Mode"),
        ParamInfo::new(p::STEREO, "Stereo", 0.0, 180.0, 180.0).unit("°"),
        ParamInfo::new(p::INVERT, "Invert", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::AMOUNT, "Amount", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::SHAPE, "Shape", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::ATTACK, "Attack", 0.0, 2000.0, 0.0).unit("ms"),
        ParamInfo::new(p::DYN_FM, "Dynamic FM", -100.0, 100.0, 0.0).unit("%").short("Dyn FM"),
        ParamInfo::new(p::HARMONIC, "Harmonic", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::VINTAGE, "Vintage", 0.0, 1.0, 0.0).choices(OFF_ON),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
    ]
}

pub struct AutoPan {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    lfo: [PhaseLfo; 2],
    /// Free-running phase accumulators (left, right).
    phase: [f64; 2],
    /// One-pole smoothed LFO values (clicks on square flanks).
    smooth: [f32; 2],
    smooth_coeff: f32,
    fresh: bool,
    amount: Smoother,
    wet: Smoother,
    dry: Smoother,
    env: EnvelopeFollower,
    quiet: usize,
    attack: f32,
    split: [Lr4; 2],
    split_c: Lr4Coeffs,
    first: bool,
    last_lfo: [f32; 2],
    /// Path in force (the target of a running cross-fade).
    cur: Path,
    /// Path being faded out.
    from: Path,
    /// Cross-fade position and length in samples; `pos == len` when idle.
    fade_pos: u32,
    fade_len: u32,
    /// Newest change that arrived during a cross-fade.
    queued: Option<Path>,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(AutoPan::new())
}

impl Default for AutoPan {
    fn default() -> Self {
        Self::new()
    }
}

/// Pan gain for LFO position `v` in −1..1 at depth `amount` in 0..1 (left
/// channel form; the right channel evaluates it on its own LFO). Between 0
/// and 1 always; exactly 1 at depth 0; equal-power cosine law at depth 1.
#[inline]
fn pan_gain(v: f32, amount: f32) -> f32 {
    let a = amount.clamp(0.0, 1.0);
    1.0 - a + a * ((v.clamp(-1.0, 1.0) + 1.0) * FRAC_PI_4).cos()
}

/// Settings that switch the signal path. All combinations are valid, so a
/// change cross-fades from the old path to the new one.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Path {
    tremolo: bool,
    invert: bool,
    harmonic: bool,
    vintage: bool,
    wave: Waveform,
}

/// Soft asymmetric saturation of the vintage tremolo; `drive` > 0.
#[inline]
fn vintage_sat(x: f32, drive: f32) -> f32 {
    let k = 1.0 + 3.0 * drive;
    let bias = 0.25 * drive;
    ((k * x + bias).tanh() - bias.tanh()) / k
}

impl AutoPan {
    pub fn new() -> Self {
        let params = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Auto Pan",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["modulation", "pan", "tremolo", "lfo", "autopan"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            lfo: [PhaseLfo::new(0x2545_F491), PhaseLfo::new(0x9E37_79B9)],
            phase: [0.0; 2],
            smooth: [0.0; 2],
            smooth_coeff: 1.0,
            fresh: true,
            amount: Smoother::new(1.0),
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            env: EnvelopeFollower::new(DetectorMode::Peak),
            quiet: 0,
            attack: 1.0,
            split: [Lr4::default(); 2],
            split_c: Lr4Coeffs::default(),
            first: true,
            last_lfo: [0.0; 2],
            cur: Path { tremolo: false, invert: false, harmonic: false, vintage: false, wave: Waveform::Sine },
            from: Path { tremolo: false, invert: false, harmonic: false, vintage: false, wave: Waveform::Sine },
            fade_pos: 0,
            fade_len: 1,
            queued: None,
        };
        c.prepare(48_000.0, 512);
        c
    }

    fn update(&mut self) {
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        self.amount.set_target(self.params.plain(p::AMOUNT) / 100.0);
        let tremolo = self.params.choice(p::MODE) == 1;
        let new = Path {
            tremolo,
            invert: self.params.on(p::INVERT),
            harmonic: tremolo && self.params.on(p::HARMONIC),
            vintage: tremolo && self.params.on(p::VINTAGE),
            wave: Waveform::from_index(self.params.choice(p::WAVE)),
        };
        if self.first {
            self.cur = new;
            self.from = new;
            self.fade_pos = self.fade_len;
            self.queued = None;
        } else if self.fading() {
            self.queued = Some(new);
        } else if new != self.cur {
            self.begin_fade(new);
        }
        if self.first {
            self.wet.snap_to_target();
            self.dry.snap_to_target();
            self.amount.snap_to_target();
            self.first = false;
        }
    }
}

impl AutoPan {
    #[inline]
    fn fading(&self) -> bool {
        self.fade_pos < self.fade_len
    }

    fn begin_fade(&mut self, new: Path) {
        self.from = self.cur;
        self.cur = new;
        self.fade_pos = 0;
        if new.harmonic && !self.from.harmonic {
            // The crossover has been idle: start it from silence.
            for s in &mut self.split {
                s.reset();
            }
        }
    }

    /// Advance the cross-fade by one sample; returns the weight of the new
    /// path (1 when idle). A change queued during a fade starts afterwards.
    #[inline]
    fn fade_weight(&mut self) -> f32 {
        if !self.fading() {
            if let Some(q) = self.queued.take()
                && q != self.cur
            {
                self.begin_fade(q);
            }
            if !self.fading() {
                return 1.0;
            }
        }
        self.fade_pos += 1;
        self.fade_pos as f32 / self.fade_len as f32
    }
}

/// Output of one path for one channel: `x` input, `v` the smoothed LFO,
/// `amount` the depth, `(lo, hi)` the harmonic split of `x`.
#[inline]
fn path_out(p: &Path, x: f32, v: f32, amount: f32, lo: f32, hi: f32) -> f32 {
    let v = if p.invert { -v } else { v };
    if !p.tremolo {
        return x * pan_gain(v, amount);
    }
    let mut u = 0.5 * (v + 1.0);
    if p.vintage {
        u = u.powf(1.6);
    }
    let g = 1.0 - amount * (1.0 - u);
    // Harmonic: low band gets g, high band 2 − g; written against the input
    // so g = 1 is exactly transparent.
    let mut y = if p.harmonic { x + (g - 1.0) * (lo - hi) } else { x * g };
    if p.vintage && amount > 0.0 {
        y = vintage_sat(y, 0.3 * amount);
    }
    y
}

impl Segmented for AutoPan {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let tr = ctx.transport;
        let sr = self.sr;
        let time_mode = self.params.choice(p::TIME_MODE);
        let div = self.params.choice(p::DIVISION);
        let hz = match time_mode {
            0 => self.params.plain(p::RATE) as f64,
            1 => 1000.0 / self.params.plain(p::TIME) as f64,
            _ => division_hz(div, tr.bpm),
        };
        let base_inc = hz / sr;
        let sync_play = time_mode == 2 && tr.playing;
        let offset = (self.params.plain(p::PHASE) / 360.0) as f64;
        // Random shapes only offer Phase (their sequences are independent).
        let spin = self.params.choice(p::STEREO_MODE) == 1 && !self.cur.wave.is_random();
        let stereo = self.params.plain(p::STEREO);
        let (off_r, ratio_r) = if spin { (0.0, 1.0 + SPIN_SPREAD * (stereo / 180.0) as f64) } else { ((stereo / 360.0) as f64, 1.0) };
        let shape = self.params.plain(p::SHAPE) / 100.0;
        let attack_ms = self.params.plain(p::ATTACK);
        let attack_step = if attack_ms > 0.0 { 1.0 / (attack_ms * 0.001 * sr as f32) } else { 1.0 };
        let dyn_fm = self.params.plain(p::DYN_FM) / 100.0;
        let rearm = (ATTACK_REARM_S * sr) as usize;
        if sync_play {
            let beats = (tr.position_beats + start as f64 / sr * tr.bpm / 60.0) / division_beats(div);
            self.phase = [beats.rem_euclid(1.0), (beats * ratio_r).rem_euclid(1.0)];
        }
        let free_fm = dyn_fm != 0.0 && !sync_play;
        let coeff = self.smooth_coeff;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let t = self.fade_weight();
            let fading = t < 1.0;
            let (cur, from) = (self.cur, self.from);
            // Input follower: rate modulation and attack onsets.
            let env = self.env.process(l.abs().max(r.abs()));
            if env > ATTACK_THRESHOLD {
                if self.quiet >= rearm && attack_ms > 0.0 {
                    self.attack = 0.0;
                }
                self.quiet = 0;
            } else {
                self.quiet = self.quiet.saturating_add(1);
            }
            self.attack = if attack_ms > 0.0 { (self.attack + attack_step).min(1.0) } else { 1.0 };

            let inc = if free_fm { base_inc * f64::from((dyn_fm * env.min(1.0) * 2.0).exp2()) } else { base_inc };
            self.phase[0] = (self.phase[0] + inc).rem_euclid(1.0);
            self.phase[1] = (self.phase[1] + inc * ratio_r).rem_euclid(1.0);
            let ph = [self.phase[0] + offset, self.phase[1] + offset + off_r];
            let mut lfo = [self.lfo[0].value(cur.wave, ph[0], shape), self.lfo[1].value(cur.wave, ph[1], shape)];
            if fading && from.wave != cur.wave {
                // Same phase again: the generator only reads it, so the
                // random sequence is not advanced twice.
                for c in 0..2 {
                    let old = self.lfo[c].value(from.wave, ph[c], shape);
                    lfo[c] = old + (lfo[c] - old) * t;
                }
            }
            if self.fresh {
                self.smooth = lfo;
                self.fresh = false;
            }
            let amount = self.amount.next() * self.attack;
            let (w, d) = (self.wet.next(), self.dry.next());
            let input = [l, r];
            let mut out = [0.0f32; 2];
            for c in 0..2 {
                self.smooth[c] += (lfo[c] - self.smooth[c]) * coeff;
                let v = self.smooth[c];
                self.last_lfo[c] = if cur.invert { -v } else { v };
                let x = input[c];
                let (lo, hi) = if cur.harmonic || (fading && from.harmonic) { self.split[c].split(&self.split_c, x) } else { (x, 0.0) };
                let new = path_out(&cur, x, v, amount, lo, hi);
                out[c] = if fading { path_out(&from, x, v, amount, lo, hi) * (1.0 - t) + new * t } else { new };
            }
            (sanitize(l * d + out[0] * w), sanitize(r * d + out[1] * w))
        });
    }
}

impl Processor for AutoPan {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.smooth_coeff = one_pole_coeff(0.7, sample_rate);
        for s in [&mut self.amount, &mut self.wet, &mut self.dry] {
            s.set_time(20.0, sample_rate);
        }
        self.env.set_times(5.0, 80.0, sample_rate);
        self.fade_len = ((FADE_MS * 0.001 * sample_rate) as u32).max(1);
        self.fade_pos = self.fade_len;
        self.split_c = Lr4Coeffs::new(SPLIT_HZ, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        if let Some(q) = self.queued.take() {
            self.cur = q;
        }
        self.from = self.cur;
        self.fade_pos = self.fade_len;
        self.phase = [0.0; 2];
        for l in &mut self.lfo {
            l.reset();
        }
        for s in &mut self.split {
            s.reset();
        }
        self.env.reset();
        self.smooth = [0.0; 2];
        self.fresh = true;
        self.quiet = usize::MAX / 2;
        self.attack = 1.0;
        self.last_lfo = [0.0; 2];
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

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.last_lfo[0], self.last_lfo[1], self.phase[0] as f32, self.env.value().min(1.0)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{BLOCK, SR, get_plain, peak, process_stereo, process_stereo_prepared, rms, set_plain, sine};
    use aitrack_dsp::Transport;

    fn noise(seed: u32) -> impl FnMut(usize) -> (f32, f32) {
        let mut n = aitrack_dsp::osc::Noise::new(seed);
        move |_| {
            let v = 0.5 * n.next();
            (v, v)
        }
    }

    fn hz_mode(c: &mut AutoPan, hz: f32) {
        set_plain(c, p::TIME_MODE, 0.0);
        set_plain(c, p::RATE, hz);
    }

    #[test]
    fn registry_params_are_stable() {
        let c = AutoPan::new();
        assert_eq!(c.info().params.len(), 18);
        assert_eq!(c.latency(), 0);
        assert_eq!(c.tail(), 0);
        assert_eq!(get_plain(&c, p::STEREO), 180.0);
        assert_eq!(get_plain(&c, p::AMOUNT), 100.0);
    }

    /// Done criterion 2.1a: at 180° and Amount 100 % the pan keeps constant
    /// power (±0.5 dB) on a mono source (centre −3 dB, extremes 0 dB), for the waves that are odd around
    /// its half cycle (the left/right gains are then exact complements).
    #[test]
    fn pan_at_180_keeps_constant_power() {
        for wave in [0.0, 1.0, 5.0] {
            let mut c = AutoPan::new();
            set_plain(&mut c, p::WAVE, wave);
            hz_mode(&mut c, 3.0);
            set_plain(&mut c, p::STEREO, 180.0);
            set_plain(&mut c, p::AMOUNT, 100.0);
            let (l, r) = process_stereo(&mut c, noise(5), 96_000);
            let mut inp = noise(5);
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for i in 0..96_000 {
                let (x, _) = inp(i);
                if i < 4800 || x.abs() < 0.05 {
                    continue;
                }
                let ratio = ((l[i] * l[i] + r[i] * r[i]) / (x * x)) as f64;
                let db = 10.0 * ratio.log10();
                lo = lo.min(db);
                hi = hi.max(db);
            }
            assert!(lo > -0.5 && hi < 0.5, "wave {wave}: {lo:.3}..{hi:.3} dB");
        }
    }

    /// The pan really moves: the channels trade level.
    #[test]
    fn pan_swings_between_channels() {
        let mut c = AutoPan::new();
        hz_mode(&mut c, 2.0);
        let (l, r) = process_stereo(&mut c, sine(440.0, 0.5), 48_000);
        let lv: Vec<f32> = l.chunks(2400).map(rms).collect();
        let rv: Vec<f32> = r.chunks(2400).map(rms).collect();
        let (lmax, lmin) = lv.iter().fold((0.0f32, f32::MAX), |(a, b), &v| (a.max(v), b.min(v)));
        assert!(lmax > 6.0 * lmin.max(1e-4), "{lmax} {lmin}");
        // When the left is loudest the right is quietest.
        let il = lv.iter().position(|&v| v == lmax).unwrap();
        assert!(rv[il] < 0.3 * lmax, "{} {lmax}", rv[il]);
    }

    /// Done criterion 2.1b: Amount 0 passes the signal unchanged, in every
    /// mode, including Harmonic and Vintage.
    #[test]
    fn amount_zero_is_transparent() {
        for (mode, harmonic, vintage) in [(0.0, 0.0, 0.0), (1.0, 0.0, 0.0), (1.0, 1.0, 0.0), (1.0, 0.0, 1.0), (1.0, 1.0, 1.0)] {
            let mut c = AutoPan::new();
            set_plain(&mut c, p::MODE, mode);
            set_plain(&mut c, p::HARMONIC, harmonic);
            set_plain(&mut c, p::VINTAGE, vintage);
            set_plain(&mut c, p::AMOUNT, 0.0);
            hz_mode(&mut c, 5.0);
            let (l, r) = process_stereo(&mut c, noise(9), 20_000);
            let mut inp = noise(9);
            for i in 0..20_000 {
                let (x, _) = inp(i);
                assert!((l[i] - x).abs() < 1e-6 && (r[i] - x).abs() < 1e-6, "mode {mode} harm {harmonic} vint {vintage} @{i}: {} vs {x}", l[i]);
            }
        }
    }

    #[test]
    fn mix_zero_is_dry() {
        let mut c = AutoPan::new();
        set_plain(&mut c, p::MIX, 0.0);
        hz_mode(&mut c, 4.0);
        let (l, _) = process_stereo(&mut c, noise(2), 8000);
        let mut inp = noise(2);
        for i in 0..8000 {
            assert!((l[i] - inp(i).0).abs() < 1e-6);
        }
    }

    #[test]
    fn tremolo_depth_follows_amount() {
        for amount in [25.0f32, 50.0, 100.0] {
            let mut c = AutoPan::new();
            set_plain(&mut c, p::MODE, 1.0);
            set_plain(&mut c, p::STEREO, 0.0);
            set_plain(&mut c, p::AMOUNT, amount);
            hz_mode(&mut c, 2.0);
            // DC-ish input makes the gain visible directly.
            let (l, _) = process_stereo(&mut c, |_| (1.0, 1.0), 48_000);
            let tail = &l[4800..];
            let (lo, hi) = tail.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
            assert!((hi - 1.0).abs() < 0.02, "hi {hi}");
            assert!((lo - (1.0 - amount / 100.0)).abs() < 0.03, "amount {amount}: lo {lo}");
        }
    }

    #[test]
    fn invert_flips_the_modulation() {
        let run = |inv: f32| {
            let mut c = AutoPan::new();
            set_plain(&mut c, p::MODE, 1.0);
            set_plain(&mut c, p::STEREO, 0.0);
            set_plain(&mut c, p::INVERT, inv);
            hz_mode(&mut c, 1.0);
            process_stereo(&mut c, |_| (1.0, 1.0), 12_000).0
        };
        let (a, b) = (run(0.0), run(1.0));
        // Quarter cycle in: the sine is at its peak, so Invert is at its trough.
        assert!(a[12_000 - 1] > 0.95 && b[12_000 - 1] < 0.1, "{} {}", a[11_999], b[11_999]);
    }

    /// With the transport running the LFO sits at the song position.
    #[test]
    fn sync_locks_phase_to_the_transport() {
        let mut c = AutoPan::new();
        set_plain(&mut c, p::TIME_MODE, 2.0);
        set_plain(&mut c, p::DIVISION, 7.0); // 1/4
        set_plain(&mut c, p::MODE, 1.0);
        set_plain(&mut c, p::STEREO, 0.0);
        c.prepare(SR, BLOCK);
        // 10.25 beats in: a quarter note cycle is 25 % through, sine at peak.
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 10.25, ..Transport::default() };
        let (l, _) = process_stereo_prepared(&mut c, |_| (1.0, 1.0), 480, &tr);
        // phase ~0.25 -> lfo +1 -> tremolo gain ~ 1.
        assert!(l[479] > 0.98, "{}", l[479]);
        let mut m = [0.0f32; 4];
        assert_eq!(c.meter(&mut m), 4);
        assert!((m[2] - 0.27).abs() < 0.01, "{}", m[2]);
        // Half a cycle later the sine is at its trough.
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 10.75, ..Transport::default() };
        let (l, _) = process_stereo_prepared(&mut c, |_| (1.0, 1.0), 480, &tr);
        assert!(l[479] < 0.05, "{}", l[479]);
    }

    #[test]
    fn free_running_lfo_has_the_set_period() {
        let mut c = AutoPan::new();
        set_plain(&mut c, p::MODE, 1.0);
        set_plain(&mut c, p::STEREO, 0.0);
        set_plain(&mut c, p::TIME_MODE, 1.0);
        set_plain(&mut c, p::TIME, 250.0); // 4 Hz
        let (l, _) = process_stereo(&mut c, |_| (1.0, 1.0), 48_000);
        // Count downward zero crossings of (gain - 0.5).
        let crossings = l.windows(2).filter(|w| w[0] >= 0.5 && w[1] < 0.5).count();
        assert!((3..=5).contains(&crossings), "{crossings}");
    }

    #[test]
    fn harmonic_alternates_the_bands() {
        let mut c = AutoPan::new();
        set_plain(&mut c, p::MODE, 1.0);
        set_plain(&mut c, p::HARMONIC, 1.0);
        set_plain(&mut c, p::STEREO, 0.0);
        hz_mode(&mut c, 2.0);
        // Tone below and above the 600 Hz split: their envelopes must be
        // out of phase.
        let lo = process_stereo(&mut c, sine(150.0, 0.5), 48_000).0;
        c.reset();
        let hi = process_stereo(&mut c, sine(3000.0, 0.5), 48_000).0;
        let lv: Vec<f32> = lo.chunks(2400).skip(2).map(rms).collect();
        let hv: Vec<f32> = hi.chunks(2400).skip(2).map(rms).collect();
        let corr: f32 = {
            let ml = lv.iter().sum::<f32>() / lv.len() as f32;
            let mh = hv.iter().sum::<f32>() / hv.len() as f32;
            lv.iter().zip(&hv).map(|(a, b)| (a - ml) * (b - mh)).sum()
        };
        assert!(corr < 0.0, "{corr}");
    }

    #[test]
    fn attack_ramps_the_depth_after_silence() {
        let mut c = AutoPan::new();
        set_plain(&mut c, p::MODE, 1.0);
        set_plain(&mut c, p::STEREO, 0.0);
        set_plain(&mut c, p::ATTACK, 500.0);
        hz_mode(&mut c, 8.0);
        // 0.5 s of silence, then DC.
        let (l, _) = process_stereo(&mut c, |i| if i < 24_000 { (0.0, 0.0) } else { (1.0, 1.0) }, 24_000 + 48_000);
        let depth = |a: usize, b: usize| {
            let s = &l[24_000 + a..24_000 + b];
            let (lo, hi) = s.iter().fold((f32::MAX, f32::MIN), |(x, y), &v| (x.min(v), y.max(v)));
            hi - lo
        };
        let early = depth(0, 4800);
        let late = depth(36_000, 48_000);
        assert!(early < 0.35 * late, "early {early} late {late}");
        assert!(late > 0.8, "{late}");
    }

    #[test]
    fn dynamic_fm_speeds_up_with_level() {
        let count = |amp: f32, fm: f32| {
            let mut c = AutoPan::new();
            set_plain(&mut c, p::MODE, 1.0);
            set_plain(&mut c, p::STEREO, 0.0);
            set_plain(&mut c, p::DYN_FM, fm);
            hz_mode(&mut c, 2.0);
            let (l, _) = process_stereo(&mut c, move |_| (amp, amp), 96_000);
            // Tremolo gain = out / in.
            l[9_600..].windows(2).filter(|w| w[0] / amp >= 0.5 && w[1] / amp < 0.5).count()
        };
        let base = count(0.8, 0.0);
        let fast = count(0.8, 100.0);
        let slow = count(0.8, -100.0);
        assert!(fast > base + 2 && slow < base, "slow {slow} base {base} fast {fast}");
    }

    #[test]
    fn stereo_spin_drifts_the_channels_apart() {
        let mut c = AutoPan::new();
        set_plain(&mut c, p::MODE, 1.0);
        set_plain(&mut c, p::STEREO_MODE, 1.0);
        set_plain(&mut c, p::STEREO, 180.0);
        hz_mode(&mut c, 2.0);
        let (l, r) = process_stereo(&mut c, |_| (1.0, 1.0), 96_000);
        // Left and right start in step and end up apart.
        assert!((l[2400] - r[2400]).abs() < 0.1);
        let diff: f32 = l[48_000..].iter().zip(&r[48_000..]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(diff > 0.3, "{diff}");
    }

    #[test]
    fn random_waves_use_independent_sequences_per_channel() {
        for wave in [6.0, 7.0, 8.0] {
            let mut c = AutoPan::new();
            set_plain(&mut c, p::MODE, 1.0);
            set_plain(&mut c, p::WAVE, wave);
            set_plain(&mut c, p::STEREO, 0.0);
            hz_mode(&mut c, 5.0);
            let (l, r) = process_stereo(&mut c, |_| (1.0, 1.0), 96_000);
            let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(diff > 0.05, "wave {wave}: {diff}");
            assert!(peak(&l) <= 1.0 + 1e-4);
        }
    }

    #[test]
    fn state_round_trip() {
        let mut a = AutoPan::new();
        set_plain(&mut a, p::WAVE, 5.0);
        set_plain(&mut a, p::AMOUNT, 42.0);
        let bytes = a.save_state();
        let mut b = AutoPan::new();
        b.load_state(&bytes);
        assert_eq!(get_plain(&b, p::WAVE), 5.0);
        assert!((get_plain(&b, p::AMOUNT) - 42.0).abs() < 1e-3);
    }

    /// Runs a 0.7-amplitude sine of `freq` Hz through `c` and returns the
    /// largest step between contiguous output samples around a parameter
    /// event delivered mid-block, and the rms of the 10 ms that follow the
    /// event. `value` of `None` runs without the event.
    fn measure(c: &mut dyn Processor, init: &[(u32, f32)], freq: f64, id: u32, value: Option<f32>) -> (f32, f32) {
        use crate::test_util::{BLOCK, SR};
        use aitrack_dsp::{AudioBuffer, Event, Transport};
        c.prepare(SR, BLOCK);
        for &(i, v) in init {
            c.set_param(i, v);
        }
        let tr = Transport { playing: true, ..Transport::default() };
        let mut buf = AudioBuffer::new(2, BLOCK);
        let mut prev = 0.0f32;
        let mut worst = 0.0f32;
        let mut window = Vec::new();
        for b in 0..60 {
            buf.set_frames(BLOCK);
            for i in 0..BLOCK {
                let n = (b * BLOCK + i) as f64;
                let v = 0.7 * (std::f64::consts::TAU * freq * n / SR).sin() as f32;
                buf.channel_mut(0)[i] = v;
                buf.channel_mut(1)[i] = v;
            }
            let ev = [Event { offset: 64, kind: EventKind::Param { id, value: value.unwrap_or(0.0) } }];
            let events: &[Event] = if b == 40 && value.is_some() { &ev } else { &[] };
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: None, events, transport: &tr };
            c.process(&mut ctx);
            for i in 0..BLOCK {
                let y = buf.channel(0)[i];
                if b >= 38 {
                    worst = worst.max((y - prev).abs());
                }
                if b >= 40 && window.len() < 480 && (b > 40 || i >= 64) {
                    window.push(y);
                }
                prev = y;
            }
        }
        (worst, crate::test_util::rms(&window))
    }

    #[test]
    fn discrete_changes_do_not_click_or_drop() {
        // (initial normalised params, param, new normalised value)
        type Case = (&'static [(u32, f32)], u32, f32);
        let cases: [Case; 9] = [
            (&[], p::MODE, 0.418),
            (&[], p::MODE, 1.0),
            (&[(p::MODE, 1.0)], p::MODE, 0.0),
            (&[], p::INVERT, 0.329),
            (&[], p::INVERT, 1.0),
            (&[(p::INVERT, 1.0)], p::INVERT, 0.0),
            (&[(p::MODE, 1.0)], p::HARMONIC, 1.0),
            (&[(p::MODE, 1.0)], p::VINTAGE, 1.0),
            (&[], p::WAVE, 0.5),
        ];
        for (init, id, value) in cases {
            let mut after = init.to_vec();
            after.push((id, value));
            // Click test, on a low sine: the steady state of either setting
            // is the yardstick.
            let (before_step, _) = measure(&mut AutoPan::new(), init, 103.0, id, None);
            let (after_step, _) = measure(&mut AutoPan::new(), &after, 103.0, id, None);
            let reference = before_step.max(after_step);
            let (changed, _) = measure(&mut AutoPan::new(), init, 103.0, id, Some(value));
            eprintln!("JUMP {id} {value}: ref {reference} changed {changed}");
            assert!(changed <= 2.5 * reference + 0.004, "param {id} -> {value}: {changed} vs reference {reference}");
            // Level test, on a 1 kHz sine (whole cycles in the window): the
            // output never falls into a hole between the two settings.
            let (_, before) = measure(&mut AutoPan::new(), init, 1000.0, id, None);
            let (_, after) = measure(&mut AutoPan::new(), &after, 1000.0, id, None);
            let (_, during) = measure(&mut AutoPan::new(), init, 1000.0, id, Some(value));
            let floor = before.min(after);
            eprintln!("LEVEL {id} {value}: before {before} after {after} during {during}");
            assert!(during >= floor * 0.891, "param {id} -> {value}: level {during} fell below {floor}");
        }
    }

    #[test]
    fn pan_never_exceeds_the_input_peak() {
        for wave in 0..9 {
            for amount in [10.0f32, 50.0, 100.0] {
                for (stereo, phase) in [(180.0f32, 0.0f32), (0.0, 90.0), (90.0, 200.0), (180.0, 45.0)] {
                    let mut c = AutoPan::new();
                    set_plain(&mut c, p::WAVE, wave as f32);
                    set_plain(&mut c, p::AMOUNT, amount);
                    set_plain(&mut c, p::STEREO, stereo);
                    set_plain(&mut c, p::PHASE, phase);
                    hz_mode(&mut c, 3.0);
                    let (l, r) = process_stereo(&mut c, |i| { let v = (i as f32 * 0.31).sin(); (v, v) }, 48_000);
                    let in_peak = (0..48_000).map(|i| (i as f32 * 0.31).sin().abs()).fold(0.0f32, f32::max);
                    let out_peak = peak(&l).max(peak(&r));
                    assert!(out_peak <= in_peak + 1e-6, "wave {wave} amount {amount} stereo {stereo} phase {phase}: {out_peak} > {in_peak}");
                }
            }
        }
    }
}
