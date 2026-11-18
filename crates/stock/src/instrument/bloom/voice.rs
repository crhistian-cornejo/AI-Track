//! One Bloom voice: the **Body** (two PolyBLEP oscillators with a pitch
//! sweep, a sine modulator on oscillator 1's phase with its own index
//! envelope, filtered noise with its own envelope and clap-style bursts,
//! drive, a state-variable or ladder filter with an envelope, and an AHD +
//! release amplifier) feeding the **Resonator**, a bank of up to
//! [`MAX_MODES`] two-pole modes tuned to one of five materials and morphed
//! between them.
//!
//! Everything a note needs is worked out once when it starts ([`Patch::new`]
//! from the genome, the macros, the velocity and the sample rate); the
//! per-sample loop is multiplies and adds, with the pitch, filter and FM
//! index updated every [`CONTROL`] samples. No allocation anywhere.

use super::genome::{Genome, g};
use crate::instrument::vault::kit::{flush, sin01, soft};
use aitrack_dsp::ladder::{Ladder, LadderKind};
use aitrack_dsp::osc::poly_blep;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};

/// Most resonator modes.
pub const MAX_MODES: usize = 24;
/// Samples between two updates of the pitch, filter and FM index.
pub const CONTROL: usize = 16;

/// The device's macro knobs, each -1..1 (0 = the genome as grown).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Macros {
    pub decay: f32,
    pub body: f32,
    pub tone: f32,
    pub grit: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct Mode {
    b0: f32,
    a1: f32,
    a2: f32,
    /// Left / right gains (modes alternate sides with the width).
    l: f32,
    r: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wave {
    Sine,
    Tri,
    Saw,
    Square,
}

fn wave_of(i: usize) -> Wave {
    match i {
        1 => Wave::Tri,
        2 => Wave::Saw,
        3 => Wave::Square,
        _ => Wave::Sine,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FilterKind {
    Svf(SvfMode),
    Ladder,
}

/// The ratio of mode `k` (0-based) of a material. `shape` is 0..4:
/// string, membrane, plate, bar, inharmonic; fractions morph between two.
pub fn mode_ratio(shape: f32, k: usize) -> f32 {
    const MEMBRANE: [f32; MAX_MODES] = [
        1.0, 1.594, 2.136, 2.296, 2.653, 2.918, 3.156, 3.501, 3.600, 3.652, 4.060, 4.154, 4.230, 4.601, 4.832, 4.903, 5.131, 5.412, 5.469, 5.650,
        5.708, 5.982, 6.153, 6.204,
    ];
    const PLATE: [f32; MAX_MODES] = [
        1.0, 2.5, 4.0, 5.0, 6.5, 8.5, 9.0, 10.0, 12.5, 13.0, 14.5, 16.0, 17.0, 18.5, 20.0, 20.5, 25.0, 25.3, 26.0, 26.5, 29.0, 29.4, 32.0, 32.5,
    ];
    const INHARM: [f32; MAX_MODES] = [
        1.0, 1.47, 2.09, 2.56, 2.76, 3.41, 4.07, 4.33, 5.17, 5.40, 6.27, 6.98, 7.21, 8.14, 8.93, 9.48, 10.6, 11.2, 11.9, 12.7, 13.8, 14.4, 15.6, 16.9,
    ];
    let one = |s: usize| -> f32 {
        match s {
            0 => (k + 1) as f32,
            1 => MEMBRANE[k],
            2 => PLATE[k],
            3 => {
                // Free-free bar: (n + ½)² over the first.
                let n = k as f32 + 1.5;
                n * n / 2.25
            }
            _ => INHARM[k],
        }
    };
    let s = shape.clamp(0.0, 4.0);
    let i = (s.floor() as usize).min(3);
    let f = s - i as f32;
    let (a, b) = (one(i), one(i + 1));
    // Geometric blend: a material halfway between two sits between them in pitch.
    a * (b / a).powf(f)
}

/// Everything one note needs, precomputed.
#[derive(Clone, Copy, Debug)]
pub struct Patch {
    base_hz: f32,
    sweep_st: f32,
    sweep_k: f32,
    w1: Wave,
    w2: Wave,
    ratio2: f32,
    g1: f32,
    g2: f32,
    fm_ratio: f32,
    fm_index: f32,
    fm_k: f32,
    noise_gain: f32,
    noise: SvfCoeffs,
    noise_k: f32,
    burst_k: f32,
    bursts: u32,
    burst_gap: u32,
    drive: f32,
    drive_norm: f32,
    filter: FilterKind,
    cutoff: f32,
    reso: f32,
    fenv_oct: f32,
    fenv_k: f32,
    attack: u32,
    hold: u32,
    decay_k: f32,
    release_k: f32,
    res_mix: f32,
    res_in: f32,
    n_modes: usize,
    modes: [Mode; MAX_MODES],
    pan_l: f32,
    pan_r: f32,
    /// Send to the scene's space.
    pub send: f32,
    gain: f32,
    sr: f32,
}

/// Bloom's performance knobs, read when a note starts. The default is the
/// identity (the genome as grown).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Perform {
    /// −1..1: attack ×⅛..×8 (and up to +20 ms at the top).
    pub attack: f32,
    /// −1..1: pitch sweep (−1 none, 0 as grown, +1 two more octaves).
    pub drop: f32,
    /// 0..2: FM depth.
    pub motion: f32,
    /// Filter brightness offset, octaves (the per-hit Vary).
    pub tone: f32,
    /// Level multiplier (the per-hit Vary).
    pub level: f32,
}

impl Default for Perform {
    fn default() -> Self {
        Self { attack: 0.0, drop: 0.0, motion: 1.0, tone: 0.0, level: 1.0 }
    }
}

impl Patch {
    /// Apply the performance knobs (nothing at all at their defaults).
    pub fn perform(&mut self, k: &Perform) {
        if *k == Perform::default() {
            return;
        }
        if k.attack != 0.0 {
            let a = k.attack.clamp(-1.0, 1.0);
            let ms = self.attack as f32 * 1000.0 / self.sr;
            let ms = ms * 2f32.powf(a * 3.0) + a.max(0.0) * 20.0;
            self.attack = (ms * 0.001 * self.sr) as u32;
        }
        let d = k.drop.clamp(-1.0, 1.0);
        self.sweep_st = if d >= 0.0 { self.sweep_st + d * 24.0 } else { self.sweep_st * (1.0 + d) };
        self.fm_index = (self.fm_index * k.motion.clamp(0.0, 2.0)).min(24.0);
        self.cutoff *= 2f32.powf(k.tone);
        self.gain *= k.level.max(0.0);
    }
}

/// Per-sample decay multiplier for a time constant of `ms`.
#[inline]
fn coef(ms: f32, sr: f32) -> f32 {
    (-1.0 / (ms.max(0.05) * 0.001 * sr)).exp()
}

/// Per-sample multiplier that falls 40 dB in `ms` (how the decay genes read).
#[inline]
fn fall40(ms: f32, sr: f32) -> f32 {
    (-4.605_17 / (ms.max(0.05) * 0.001 * sr)).exp()
}

impl Patch {
    /// `transpose` in semitones (chromatic play), `velocity` 0..1, `vel_amount`
    /// how much velocity changes the level (0..1), `gain` the sound's level
    /// normalisation.
    pub fn new(genome: &Genome, sr: f32, transpose: f32, velocity: f32, vel_amount: f32, m: &Macros, gain: f32) -> Self {
        let gn = genome.folded();
        let p = |k: usize| gn.plain(k);
        let vel = velocity.clamp(0.0, 1.0);
        let vt = p(g::VEL_TONE) * (vel - 0.75);
        let decay_mul = 2f32.powf(m.decay.clamp(-1.0, 1.0) * 2.0);
        let tone = m.tone.clamp(-1.0, 1.0);
        let grit = m.grit.clamp(-1.0, 1.0);
        let nyq = sr * 0.45;

        let base_hz = (440.0 * 2f32.powf((p(g::PITCH) + transpose - 69.0) / 12.0)).clamp(8.0, nyq);
        let ratio2 = p(g::OSC2_RATIO) * 2f32.powf(p(g::OSC2_DETUNE) / 1200.0);
        let mix = p(g::OSC_MIX);
        let tone_level = p(g::TONE_LEVEL);
        let fm_index = (p(g::FM_INDEX) * (1.0 + vt * 1.5) * 2f32.powf(grit * 1.2)).clamp(0.0, 24.0);

        let noise_mode = match gn.index(g::NOISE_MODE) {
            0 => SvfMode::Lowpass,
            2 => SvfMode::Highpass,
            _ => SvfMode::Bandpass,
        };
        let noise_freq = (p(g::NOISE_FREQ) * 2f32.powf(tone * 1.5 + vt)).clamp(40.0, nyq);
        let noise = SvfCoeffs::new(noise_mode, noise_freq, p(g::NOISE_Q), 0.0, f64::from(sr));
        let bursts = gn.index(g::BURSTS) as u32;
        let gap_ms = p(g::BURST_GAP);
        let noise_ms = p(g::NOISE_DECAY) * decay_mul;

        let drive = (p(g::DRIVE) + grit * 0.45).clamp(0.0, 1.0);
        let filter = match gn.index(g::FILTER) {
            1 => FilterKind::Svf(SvfMode::Bandpass),
            2 => FilterKind::Svf(SvfMode::Highpass),
            3 => FilterKind::Ladder,
            _ => FilterKind::Svf(SvfMode::Lowpass),
        };
        let cutoff = p(g::CUTOFF) * 2f32.powf(tone * 3.0 + vt * 3.0 + grit.min(0.0) * 1.5);
        let reso = (p(g::RESO) + grit.max(0.0) * 0.2).clamp(0.0, 0.97);

        let res_mix = (p(g::RES_MIX) + m.body.clamp(-1.0, 1.0) * 0.5).clamp(0.0, 1.0);
        let n_modes = (p(g::RES_MODES).round() as usize).clamp(1, MAX_MODES);
        let mut modes = [Mode::default(); MAX_MODES];
        let mut used = 0;
        if res_mix > 0.001 {
            let res_hz = base_hz * 2f32.powf(p(g::RES_TUNE) / 12.0);
            let shape = p(g::RES_SHAPE);
            let t60 = p(g::RES_DECAY) * decay_mul;
            let damp = p(g::RES_DAMP);
            let bright = (p(g::RES_BRIGHT) + tone * 0.4).clamp(0.0, 1.0);
            let pos = 0.03 + 0.47 * p(g::RES_POS);
            let width = p(g::WIDTH);
            let mut norm = 0.0;
            for k in 0..n_modes {
                let ratio = mode_ratio(shape, k);
                let f = res_hz * ratio;
                if f >= nyq {
                    break;
                }
                let w = std::f32::consts::TAU * f / sr;
                let t = (t60 * ratio.powf(-damp * 1.4)).max(5.0);
                // 60 dB down after t ms.
                let r = (-6.907_755 / (t * 0.001 * sr)).exp();
                let amp = (std::f32::consts::PI * (k + 1) as f32 * pos).sin().abs().max(0.08) * ratio.powf(-(1.0 - bright) * 1.3);
                norm += amp * amp;
                let side = if k % 2 == 0 { -1.0 } else { 1.0 } * width * 0.7;
                modes[k] = Mode { b0: amp * w.sin() * (1.0 - r).sqrt() * 2.2, a1: 2.0 * r * w.cos(), a2: -r * r, l: 1.0 - side, r: 1.0 + side };
                used = k + 1;
            }
            let inv = 1.0 / norm.sqrt().max(1e-3);
            for md in &mut modes[..used] {
                md.b0 *= inv;
            }
        }

        let pan = p(g::PAN);
        let pan_a = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        let level = 10f32.powf(p(g::LEVEL) / 20.0);
        let vel_gain = 1.0 - vel_amount.clamp(0.0, 1.0) * (1.0 - (0.15 + 0.85 * vel * vel));
        Self {
            base_hz,
            sweep_st: p(g::SWEEP),
            sweep_k: coef(p(g::SWEEP_T), sr),
            w1: wave_of(gn.index(g::OSC1)),
            w2: wave_of(gn.index(g::OSC2)),
            ratio2,
            g1: tone_level * (1.0 - mix),
            g2: tone_level * mix,
            fm_ratio: p(g::FM_RATIO),
            fm_index,
            fm_k: coef(p(g::FM_DECAY) * decay_mul, sr),
            noise_gain: p(g::NOISE_LEVEL) * 1.6,
            noise,
            noise_k: fall40(noise_ms, sr),
            burst_k: fall40(noise_ms.min(gap_ms * 1.5), sr),
            bursts,
            burst_gap: (gap_ms * 0.001 * sr).max(1.0) as u32,
            drive,
            drive_norm: 1.0 / (1.0 + drive * 1.5),
            filter,
            cutoff,
            reso,
            fenv_oct: p(g::FILT_ENV),
            fenv_k: coef(p(g::FILT_DECAY) * decay_mul, sr),
            attack: (p(g::ATTACK) * 0.001 * sr) as u32,
            hold: (p(g::HOLD) * 0.001 * sr) as u32,
            decay_k: fall40(p(g::DECAY) * decay_mul, sr),
            release_k: fall40(p(g::RELEASE) * decay_mul.sqrt(), sr),
            res_mix: if used > 0 { res_mix } else { 0.0 },
            res_in: 0.3 + 2.7 * p(g::RES_DRIVE),
            n_modes: used,
            modes,
            pan_l: pan_a.cos() * std::f32::consts::SQRT_2,
            pan_r: pan_a.sin() * std::f32::consts::SQRT_2,
            send: p(g::SEND),
            gain: gain * level * vel_gain * 0.5,
            sr,
        }
    }

    /// How long the note sounds at most, in samples (a ceiling for the
    /// offline renders).
    pub fn length_hint(&self) -> usize {
        let t = |k: f32| if k >= 1.0 { 0.0 } else { -1.0 / k.ln() };
        let amp = self.attack as f32 + self.hold as f32 + 7.0 * t(self.decay_k);
        let modes = self.modes[..self.n_modes].iter().map(|m| {
            let r = (-m.a2).sqrt();
            if r >= 1.0 { 0.0 } else { -7.0 / r.ln() }
        });
        modes.fold(amp, f32::max) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Attack,
    Hold,
    Decay,
    Release,
    Off,
}

/// One playing note.
#[derive(Clone, Copy, Debug)]
pub struct Voice {
    pub patch: Patch,
    /// Which sound of the scene it plays.
    pub sound: usize,
    pub key: u8,
    /// Note-ons since the device started (oldest voice is stolen first).
    pub age: u64,
    stage: Stage,
    counter: u32,
    amp: f32,
    /// The release multiplier (1 until the key is let go).
    rel: f32,
    pitch_env: f32,
    fm_env: f32,
    noise_env: f32,
    filt_env: f32,
    bursts_left: u32,
    burst_count: u32,
    p1: f32,
    p2: f32,
    pm: f32,
    last1: f32,
    noise_rng: u32,
    noise_state: SvfState,
    svf: SvfState,
    svf_coeffs: SvfCoeffs,
    ladder: Ladder,
    modes: [[f32; 2]; MAX_MODES],
    inc1: f32,
    inc2: f32,
    incm: f32,
    index: f32,
    ctl: usize,
    /// A fast fade out (stolen or choked): gain multiplier per sample, 1 = none.
    fade: f32,
    fade_gain: f32,
    quiet: u32,
    /// Live measures for the visual: the voice's envelope, the energy of its
    /// output and of its first difference (a centroid estimate), and of the
    /// noise part.
    pub level: f32,
    pub e_sig: f32,
    pub e_diff: f32,
    pub e_noise: f32,
    prev_out: f32,
    pub active: bool,
}

impl Voice {
    /// A silent voice to copy from. The first call builds it (the ladder
    /// designs its oversampling filters, which allocates): make it off the
    /// audio thread, as `Bloom::new` does.
    pub fn blank() -> Self {
        static BLANK: std::sync::OnceLock<Voice> = std::sync::OnceLock::new();
        *BLANK.get_or_init(|| {
            let patch = Patch::new(&Genome::default(), 48_000.0, 0.0, 1.0, 1.0, &Macros::default(), 1.0);
            Voice::idle(patch)
        })
    }

    pub fn idle(patch: Patch) -> Self {
        Self {
            patch,
            sound: 0,
            key: 0,
            age: 0,
            stage: Stage::Off,
            counter: 0,
            amp: 0.0,
            rel: 1.0,
            pitch_env: 0.0,
            fm_env: 0.0,
            noise_env: 0.0,
            filt_env: 0.0,
            bursts_left: 0,
            burst_count: 0,
            p1: 0.0,
            p2: 0.0,
            pm: 0.0,
            last1: 0.0,
            noise_rng: 1,
            noise_state: SvfState::default(),
            svf: SvfState::default(),
            svf_coeffs: SvfCoeffs::passthrough(),
            ladder: Ladder::new(LadderKind::Transistor),
            modes: [[0.0; 2]; MAX_MODES],
            inc1: 0.0,
            inc2: 0.0,
            incm: 0.0,
            index: 0.0,
            ctl: 0,
            fade: 1.0,
            fade_gain: 1.0,
            quiet: 0,
            level: 0.0,
            e_sig: 0.0,
            e_diff: 0.0,
            e_noise: 0.0,
            prev_out: 0.0,
            active: false,
        }
    }

    /// Start a note. `noise_seed` makes every hit of the same sound alike.
    pub fn start(&mut self, patch: Patch, sound: usize, key: u8, age: u64, noise_seed: u32) {
        let sr = patch.sr;
        self.patch = patch;
        self.sound = sound;
        self.key = key;
        self.age = age;
        self.stage = if patch.attack > 0 { Stage::Attack } else { Stage::Hold };
        self.counter = 0;
        self.amp = if patch.attack > 0 { 0.0 } else { 1.0 };
        self.rel = 1.0;
        self.pitch_env = 1.0;
        self.fm_env = 1.0;
        self.noise_env = 1.0;
        self.filt_env = 1.0;
        self.bursts_left = patch.bursts;
        self.burst_count = 0;
        self.p1 = 0.0;
        self.p2 = 0.25;
        self.pm = 0.0;
        self.last1 = 0.0;
        self.noise_rng = noise_seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453) | 1;
        self.noise_state.reset();
        self.svf.reset();
        self.ladder.set_sample_rate(sr);
        self.ladder.reset();
        self.modes = [[0.0; 2]; MAX_MODES];
        self.ctl = 0;
        self.fade = 1.0;
        self.fade_gain = 1.0;
        self.quiet = 0;
        self.level = 0.0;
        self.e_sig = 0.0;
        self.e_diff = 0.0;
        self.e_noise = 0.0;
        self.prev_out = 0.0;
        self.active = true;
    }

    /// The key was released: go to the release stage.
    pub fn release(&mut self) {
        if self.active && self.stage != Stage::Off {
            self.stage = Stage::Release;
        }
    }

    /// Fade out over `ms` (voice stealing, choke groups).
    pub fn kill(&mut self, ms: f32) {
        if self.active {
            self.fade = coef(ms * 0.25, self.patch.sr);
        }
    }

    pub fn is_fading(&self) -> bool {
        self.fade < 1.0
    }

    #[inline]
    fn noise(&mut self) -> f32 {
        let mut x = self.noise_rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.noise_rng = x;
        (x >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
    }

    fn control(&mut self) {
        let p = &self.patch;
        let ratio = 2f32.powf(p.sweep_st * self.pitch_env / 12.0);
        let hz = (p.base_hz * ratio).min(p.sr * 0.45);
        self.inc1 = hz / p.sr;
        self.inc2 = (hz * p.ratio2).min(p.sr * 0.45) / p.sr;
        self.incm = (hz * p.fm_ratio).min(p.sr * 0.45) / p.sr;
        self.index = p.fm_index * self.fm_env / std::f32::consts::TAU;
        let fc = (p.cutoff * 2f32.powf(p.fenv_oct * self.filt_env)).clamp(20.0, p.sr * 0.45);
        match p.filter {
            FilterKind::Svf(mode) => self.svf_coeffs = SvfCoeffs::new(mode, fc, 0.6 + p.reso * 11.0, 0.0, f64::from(p.sr)),
            FilterKind::Ladder => self.ladder.set_params(fc, p.reso * 0.92),
        }
    }

    #[inline]
    fn wave(w: Wave, t: f32, dt: f32) -> f32 {
        match w {
            Wave::Sine => sin01(t),
            Wave::Tri => 1.0 - 4.0 * (t - 0.5).abs(),
            Wave::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            Wave::Square => {
                let y = if t < 0.5 { 1.0 } else { -1.0 };
                y + poly_blep(t, dt) - poly_blep((t + 0.5).fract(), dt)
            }
        }
    }

    /// Add `out_l.len()` samples to the outputs (and the space send).
    /// Returns false once the voice has ended.
    pub fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32], send: &mut [f32]) -> bool {
        if !self.active {
            return false;
        }
        let n = out_l.len().min(out_r.len()).min(send.len());
        let attack_step = if self.patch.attack > 0 { 1.0 / self.patch.attack as f32 } else { 1.0 };
        let mut peak = 0.0f32;
        let (mut e_sig, mut e_diff, mut e_noise) = (0.0f32, 0.0f32, 0.0f32);
        for i in 0..n {
            if self.ctl == 0 {
                self.control();
                self.ctl = CONTROL;
            }
            self.ctl -= 1;
            let p = self.patch;

            // Amplifier envelope.
            match self.stage {
                Stage::Attack => {
                    self.amp += attack_step;
                    if self.amp >= 1.0 {
                        self.amp = 1.0;
                        self.stage = Stage::Hold;
                        self.counter = 0;
                    }
                }
                Stage::Hold => {
                    self.counter += 1;
                    if self.counter >= p.hold {
                        self.stage = Stage::Decay;
                    }
                }
                Stage::Decay => self.amp *= p.decay_k,
                Stage::Release => {
                    self.amp *= p.release_k.min(p.decay_k);
                    self.rel *= p.release_k;
                }
                Stage::Off => self.amp = 0.0,
            }
            self.pitch_env *= p.sweep_k;
            self.fm_env *= p.fm_k;
            self.filt_env *= p.fenv_k;

            // Noise with bursts.
            if self.bursts_left > 0 {
                self.burst_count += 1;
                if self.burst_count >= p.burst_gap {
                    self.burst_count = 0;
                    self.bursts_left -= 1;
                    self.noise_env = 1.0;
                }
                self.noise_env *= p.burst_k;
            } else {
                self.noise_env *= p.noise_k;
            }
            let nz = self.noise();
            let noise = self.noise_state.process(&p.noise, nz) * self.noise_env * p.noise_gain;

            // Oscillators; osc 1 carries the FM.
            let pm_off = if self.index > 1e-5 { self.index * sin01(self.pm) } else { 0.0 };
            let ph1 = (self.p1 + pm_off).rem_euclid(1.0);
            let dt1 = (ph1 - self.last1).rem_euclid(1.0).clamp(self.inc1 * 0.25, 0.5);
            self.last1 = ph1;
            let o1 = Self::wave(p.w1, ph1, dt1);
            let o2 = if p.g2 > 1e-4 { Self::wave(p.w2, self.p2, self.inc2.max(1e-6)) } else { 0.0 };
            self.p1 += self.inc1;
            if self.p1 >= 1.0 {
                self.p1 -= 1.0;
            }
            self.p2 += self.inc2;
            if self.p2 >= 1.0 {
                self.p2 -= 1.0;
            }
            self.pm += self.incm;
            if self.pm >= 1.0 {
                self.pm -= 1.0;
            }

            let tonal = (o1 * p.g1 + o2 * p.g2) * self.amp;
            let noisy = noise * self.rel * if self.stage == Stage::Attack { self.amp } else { 1.0 };
            let mut x = tonal + noisy;
            if p.drive > 0.005 {
                x = soft(x * (1.0 + p.drive * 14.0)) * p.drive_norm;
            }
            x = match p.filter {
                FilterKind::Svf(_) => self.svf.process(&self.svf_coeffs, x),
                FilterKind::Ladder => self.ladder.process(x),
            };

            // Resonator.
            let (mut l, mut r) = (x, x);
            if p.res_mix > 0.0 {
                let ex = x * p.res_in;
                let (mut rl, mut rr) = (0.0, 0.0);
                for (md, st) in p.modes[..p.n_modes].iter().zip(self.modes.iter_mut()) {
                    let y = md.b0 * ex + md.a1 * st[0] + md.a2 * st[1];
                    st[1] = st[0];
                    st[0] = y;
                    rl += y * md.l;
                    rr += y * md.r;
                }
                let dry = 1.0 - p.res_mix;
                l = x * dry + soft(rl) * p.res_mix;
                r = x * dry + soft(rr) * p.res_mix;
            }

            if self.fade < 1.0 {
                self.fade_gain *= self.fade;
            }
            let gain = p.gain * self.fade_gain;
            let (ol, or) = (l * gain * p.pan_l, r * gain * p.pan_r);
            out_l[i] += ol;
            out_r[i] += or;
            send[i] += (ol + or) * 0.5 * p.send;
            let mono = (ol + or) * 0.5;
            peak = peak.max(mono.abs());
            let d = mono - self.prev_out;
            self.prev_out = mono;
            e_sig += mono * mono;
            e_diff += d * d;
            let nz_out = noisy * gain;
            e_noise += nz_out * nz_out;
        }
        // Flush tiny states (no denormals after the tail).
        self.amp = flush(self.amp);
        self.noise_env = flush(self.noise_env);
        self.rel = flush(self.rel);
        for st in &mut self.modes {
            st[0] = flush(st[0]);
            st[1] = flush(st[1]);
        }
        let a = 0.6;
        self.e_sig = self.e_sig * a + e_sig * (1.0 - a);
        self.e_diff = self.e_diff * a + e_diff * (1.0 - a);
        self.e_noise = self.e_noise * a + e_noise * (1.0 - a);
        self.level = self.amp.max(if self.patch.res_mix > 0.0 { (peak * 4.0).min(1.0) } else { 0.0 }) * self.fade_gain;
        // The note is over once its envelope is down and the output stayed
        // quiet for a while (the resonator may still ring after the body).
        let env_done = matches!(self.stage, Stage::Decay | Stage::Release) && self.amp < 1e-4 && self.noise_env * self.rel * self.patch.noise_gain < 1e-4 && self.bursts_left == 0;
        if (env_done || self.fade_gain < 1e-4) && peak < 2e-5 {
            self.quiet += n as u32;
        } else {
            self.quiet = 0;
        }
        if self.quiet > (self.patch.sr * 0.02) as u32 || self.fade_gain < 1e-5 {
            self.active = false;
            self.stage = Stage::Off;
            self.level = 0.0;
        }
        self.active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::bloom::genome::Role;

    fn render(g: &Genome, n: usize) -> Vec<f32> {
        crate::instrument::vault::kit::init();
        let patch = Patch::new(g, 48_000.0, 0.0, 1.0, 1.0, &Macros::default(), 1.0);
        let mut v = Voice::blank();
        v.start(patch, 0, 36, 0, 1);
        let (mut l, mut r, mut s) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
        for c in (0..n).step_by(64) {
            let e = (c + 64).min(n);
            v.render(&mut l[c..e], &mut r[c..e], &mut s[c..e]);
        }
        l.iter().zip(&r).map(|(a, b)| (a + b) * 0.5).collect()
    }

    #[test]
    fn materials_start_at_the_fundamental_and_rise() {
        for s in [0.0, 1.0, 1.5, 2.0, 3.0, 4.0] {
            assert!((mode_ratio(s, 0) - 1.0).abs() < 1e-5);
            for k in 1..MAX_MODES {
                assert!(mode_ratio(s, k) > mode_ratio(s, k - 1) * 0.999, "shape {s} mode {k}");
            }
        }
        assert_eq!(mode_ratio(0.0, 3), 4.0);
    }

    #[test]
    fn a_body_hit_sounds_and_ends() {
        let mut g = Genome::neutral(Role::Kick);
        g.set(g::PITCH, 33.0);
        g.set(g::SWEEP, 24.0);
        g.set(g::DECAY, 150.0);
        let out = render(&g, 48_000);
        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.05, "{peak}");
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(out[40_000..].iter().all(|v| v.abs() < 1e-3), "it has died away");
    }

    #[test]
    fn the_resonator_rings_after_a_short_body() {
        let mut g = Genome::neutral(Role::Bell);
        g.set(g::PITCH, 69.0);
        g.set(g::DECAY, 15.0);
        g.set(g::RES_MIX, 1.0);
        g.set(g::RES_DECAY, 1500.0);
        g.set(g::RES_SHAPE, 3.0);
        let out = render(&g, 48_000);
        let late: f32 = out[24_000..30_000].iter().map(|v| v * v).sum();
        let mut dry = g;
        dry.set(g::RES_MIX, 0.0);
        let out_dry = render(&dry, 48_000);
        let late_dry: f32 = out_dry[24_000..30_000].iter().map(|v| v * v).sum();
        assert!(late > late_dry * 100.0 + 1e-6, "{late} vs {late_dry}");
    }

    #[test]
    fn noise_bursts_hit_several_times() {
        let mut g = Genome::neutral(Role::Clap);
        g.set(g::TONE_LEVEL, 0.0);
        g.set(g::NOISE_LEVEL, 1.0);
        g.set(g::BURSTS, 3.0);
        g.set(g::BURST_GAP, 10.0);
        g.set(g::NOISE_DECAY, 200.0);
        g.set(g::DECAY, 300.0);
        let out = render(&g, 4_800);
        // Short-time energy rises again at each burst (every 480 samples).
        let e = |a: usize| out[a..a + 96].iter().map(|v| v * v).sum::<f32>();
        assert!(e(480) > e(380) * 1.5, "{} {}", e(480), e(380));
        assert!(e(960) > e(860) * 1.5);
    }
}
