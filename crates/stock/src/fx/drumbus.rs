//! `fx.drumbus` — drum bus processor: six small stages in series, with a
//! synthesised low end that is locked to the hits.
//!
//! ```text
//! Trim → Drive → Crunch → Compress → Transients → Boom → Damp → Mix → Output
//! ```
//!
//! * **Drive** — three curves (Soft, Medium, Hard) on a 2× oversampler with
//!   ADAA1. Pre-gain 0…+18 dB, level-matched at −6 dBFS so turning it up adds
//!   density rather than just volume.
//! * **Crunch** — the part of the signal above 500 Hz goes through a clamped
//!   sine shaper and only its *distortion* (shaped minus linear) is added back,
//!   so quiet material is untouched and loud highs grow grit.
//! * **Compress** — one knob over a fixed 4:1 bus compressor with a 10 ms
//!   attack and automatic release (the detector and ballistics of
//!   [`fx::buscomp`](super::buscomp)). The threshold falls with the knob and
//!   half of the reduction is made up.
//! * **Transients** — attack/sustain shaper keyed on the level difference of a
//!   fast and a slow envelope: positive pushes onsets, negative softens them.
//! * **Boom** — a sine oscillator tuned to 30…120 Hz whose level follows the
//!   envelope of the input's content under 150 Hz (a 150 Hz low-pass, mono).
//!   When a new hit is detected (a rise of the low band over its own recent
//!   level, and only if the lows are a fair share of the whole input, so hats
//!   and clicks do not fire it) the oscillator is restarted **in phase with
//!   the hit**: the phase is read from the filtered low band (value and slope)
//!   and, once the filter has settled on the hit (3 ms), a short lock window
//!   pulls the oscillator onto the phase of the input itself (the measured
//!   phase plus the phase lag of the key filter). So the boom adds to the
//!   kick of either polarity at any position in the bar instead of beating
//!   against it. A new trigger crossfades over 5 ms from the previous
//!   oscillator, so overlapping tails do not click. `Decay` is the release of
//!   the envelope.
//! * **Damp** — 2nd-order low-pass, 1…20 kHz (off at 20 kHz).
//!
//! Every stage has its own on/off switch and is inert at zero, so a stage
//! set to 0 (or switched off) costs no sound. The 2× oversampler is always in
//! the wet path, which gives the device a constant latency; the dry signal of
//! `Mix` is delayed to match and so is the key of the Boom.
//!
//! Meter layout (135 values): `[0]` gain reduction of Compress in dB (≤ 0,
//! peak-held, 40 dB/s fall), `[1]` input peak dBFS, `[2]` output peak dBFS,
//! `[3]` latency in samples, `[4]` current Boom level (linear, after Amount),
//! `[5]` Boom trigger counter (wraps at 1024; changes on every hit), `[6]`
//! gain applied by Transients in dB, `[7..71)` Boom level history (64
//! buckets of 10 ms, oldest first, linear peak) and `[71..135)` the same for
//! the input's low band (the key of the Boom, linear peak).

use super::buscomp::BusGain;
use super::saturator::shapes::{Adaa, Curve};
use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower, PeakHold};
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::smooth::{Smoother, one_pole_coeff};
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.drumbus";

pub mod p {
    pub const TRIM: u32 = 0;
    pub const DRIVE: u32 = 1;
    pub const DRIVE_TYPE: u32 = 2;
    pub const CRUNCH: u32 = 3;
    pub const DAMP: u32 = 4;
    pub const TRANSIENTS: u32 = 5;
    pub const BOOM_AMOUNT: u32 = 6;
    pub const BOOM_FREQ: u32 = 7;
    pub const BOOM_DECAY: u32 = 8;
    pub const COMPRESS: u32 = 9;
    pub const MIX: u32 = 10;
    pub const OUTPUT: u32 = 11;
    /// Stage switches (1 = on).
    pub const DRIVE_ON: u32 = 12;
    pub const CRUNCH_ON: u32 = 13;
    pub const COMP_ON: u32 = 14;
    pub const TRANS_ON: u32 = 15;
    pub const BOOM_ON: u32 = 16;
    pub const DAMP_ON: u32 = 17;
}

/// `DRIVE_TYPE` values.
pub mod drive_type {
    pub const SOFT: f32 = 0.0;
    pub const MEDIUM: f32 = 1.0;
    pub const HARD: f32 = 2.0;
}

const OFF_ON: &[&str] = &["Off", "On"];
const DRIVE_TYPES: &[&str] = &["Soft", "Medium", "Hard"];
/// Largest drive pre-gain.
const DRIVE_MAX_DB: f32 = 18.0;
/// Level at which the drive keeps its output equal to its input.
const DRIVE_REF: f32 = 0.5;
const CRUNCH_HZ: f32 = 500.0;
/// Boom key: low-pass corner, Hz.
const BOOM_LP_HZ: f32 = 150.0;
/// Quietest low band (linear) that can trigger the Boom (≈ −48 dBFS).
const TRIGGER_FLOOR: f32 = 0.004;
/// Fastest retrigger, ms.
const REFRACTORY_MS: f32 = 30.0;
/// Oscillator crossfade on a new trigger, ms.
const CROSSFADE_MS: f32 = 5.0;
/// The lows must reach this share of the whole input's peak to fire a hit.
const LOW_SHARE: f32 = 0.3;
/// After a trigger the key filter needs this long before its phase is
/// trustworthy, then the lock window pulls the oscillator onto it.
const SETTLE_MS: f32 = 3.0;
const LOCK_MS: f32 = 15.0;
/// Fraction of the phase error corrected per sample inside the lock window.
const LOCK_GAIN: f32 = 0.02;
const GR_FALL_DB_S: f32 = 40.0;
/// Meter history: buckets and bucket length.
const HISTORY: usize = 64;
const BUCKET_MS: f32 = 10.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::TRIM, "Trim", -12.0, 12.0, 0.0).unit("dB"),
        ParamInfo::new(p::DRIVE, "Drive", 0.0, 100.0, 20.0).unit("%"),
        ParamInfo::new(p::DRIVE_TYPE, "Drive Type", 0.0, 2.0, drive_type::MEDIUM).choices(DRIVE_TYPES).short("Type"),
        ParamInfo::new(p::CRUNCH, "Crunch", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::DAMP, "Damp", 1_000.0, 20_000.0, 20_000.0).unit("Hz").log(),
        ParamInfo::new(p::TRANSIENTS, "Transients", -100.0, 100.0, 0.0).unit("%").short("Trans"),
        ParamInfo::new(p::BOOM_AMOUNT, "Boom", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::BOOM_FREQ, "Boom Freq", 30.0, 120.0, 55.0).unit("Hz").log().short("Freq"),
        ParamInfo::new(p::BOOM_DECAY, "Boom Decay", 50.0, 800.0, 300.0).unit("ms").log().short("Decay"),
        ParamInfo::new(p::COMPRESS, "Compress", 0.0, 100.0, 25.0).unit("%").short("Comp"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::DRIVE_ON, "Drive On", 0.0, 1.0, 1.0).choices(OFF_ON).short("Drv On"),
        ParamInfo::new(p::CRUNCH_ON, "Crunch On", 0.0, 1.0, 1.0).choices(OFF_ON).short("Crn On"),
        ParamInfo::new(p::COMP_ON, "Compress On", 0.0, 1.0, 1.0).choices(OFF_ON).short("Cmp On"),
        ParamInfo::new(p::TRANS_ON, "Transients On", 0.0, 1.0, 1.0).choices(OFF_ON).short("Trn On"),
        ParamInfo::new(p::BOOM_ON, "Boom On", 0.0, 1.0, 1.0).choices(OFF_ON).short("Bm On"),
        ParamInfo::new(p::DAMP_ON, "Damp On", 0.0, 1.0, 1.0).choices(OFF_ON).short("Dmp On"),
    ]
}

/// The curve of a `DRIVE_TYPE` index (soft → hard knee).
pub fn drive_curve(choice: usize) -> Curve {
    match choice {
        0 => Curve::Transistor,
        1 => Curve::Tanh,
        _ => Curve::Clip,
    }
}

/// Output gain that keeps a [`DRIVE_REF`]-level signal where it was when the
/// pre-gain is `g`.
fn drive_makeup(curve: Curve, g: f32) -> f32 {
    let y = curve.f((g * DRIVE_REF) as f64, 0.0) as f32;
    if y > 1e-6 { DRIVE_REF / y } else { 1.0 }
}

/// Boom oscillator locked to the hits of the low band.
#[derive(Clone, Copy, Debug)]
struct Boom {
    lp: SvfCoeffs,
    lp_st: SvfState,
    hold: PeakHold,
    fast: f32,
    fast_release: f32,
    /// Peak of the whole mono input, for the low-share test.
    wide: f32,
    slow: f32,
    slow_coeff: f32,
    lo_prev: f32,
    refractory: u32,
    refractory_len: u32,
    /// Samples since the last trigger while the lock window is open.
    lock_age: u32,
    settle_len: u32,
    lock_len: u32,
    /// Oscillator step, rad/sample, and the phase lag of the key filter at
    /// the oscillator frequency.
    omega: f32,
    lag: f32,
    phase: [f32; 2],
    cur: usize,
    fade: f32,
    fade_step: f32,
    prev_env: f32,
    /// Level the outgoing oscillator keeps while it crossfades out.
    old_env: f32,
    /// True from a trigger until the envelope has died away: nothing sounds
    /// without a hit, whatever leaks into the key.
    active: bool,
    /// Counts triggers, for the UI.
    triggers: u32,
}

impl Boom {
    fn new() -> Self {
        Self {
            lp: SvfCoeffs::passthrough(),
            lp_st: SvfState::default(),
            hold: PeakHold::new(),
            fast: 0.0,
            fast_release: 1.0,
            wide: 0.0,
            slow: 0.0,
            slow_coeff: 1.0,
            lo_prev: 0.0,
            refractory: 0,
            refractory_len: 0,
            lock_age: u32::MAX,
            settle_len: 0,
            lock_len: 0,
            omega: 0.0,
            lag: 0.0,
            phase: [0.0; 2],
            cur: 0,
            fade: 1.0,
            fade_step: 1.0,
            prev_env: 0.0,
            old_env: 0.0,
            active: false,
            triggers: 0,
        }
    }

    fn prepare(&mut self, sr: f64) {
        self.lp = SvfCoeffs::new(SvfMode::Lowpass, BOOM_LP_HZ, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
        self.fast_release = one_pole_coeff(8.0, sr);
        self.slow_coeff = one_pole_coeff(40.0, sr);
        self.refractory_len = (REFRACTORY_MS as f64 * 0.001 * sr) as u32;
        self.settle_len = (SETTLE_MS as f64 * 0.001 * sr) as u32;
        self.lock_len = (LOCK_MS as f64 * 0.001 * sr) as u32;
        self.fade_step = 1.0 / ((CROSSFADE_MS as f64 * 0.001 * sr) as f32).max(1.0);
        self.reset();
    }

    /// `freq` Hz oscillator, `decay_ms` envelope release.
    fn set(&mut self, freq: f32, decay_ms: f32, sr: f64) {
        self.omega = std::f32::consts::TAU * freq / sr as f32;
        // Hold: half a period of the lowest note, so |lo| has no ripple.
        self.hold.set(14.0, decay_ms, sr);
        let (re, im) = self.lp.response(freq as f64, sr);
        self.lag = (-im.atan2(re)) as f32;
    }

    fn reset(&mut self) {
        self.lp_st.reset();
        self.hold.reset();
        self.fast = 0.0;
        self.wide = 0.0;
        self.slow = 0.0;
        self.lo_prev = 0.0;
        self.refractory = 0;
        self.lock_age = u32::MAX;
        self.fade = 1.0;
        self.prev_env = 0.0;
        self.active = false;
    }

    /// One mono key sample → `(oscillator × envelope, envelope, |key|)`.
    #[inline]
    fn process(&mut self, m: f32) -> (f32, f32, f32) {
        let lo = self.lp_st.process(&self.lp, m);
        let a = lo.abs();
        let env = self.hold.process(a);
        let d = lo - self.lo_prev;
        self.lo_prev = lo;
        if a > self.fast {
            self.fast = a;
        } else {
            self.fast += (a - self.fast) * self.fast_release;
        }
        let w = m.abs();
        if w > self.wide {
            self.wide = w;
        } else {
            self.wide += (w - self.wide) * self.fast_release;
        }
        if self.refractory > 0 {
            self.refractory -= 1;
        } else if self.fast > TRIGGER_FLOOR && self.fast > 2.0 * self.slow && self.fast > LOW_SHARE * self.wide {
            // A hit: restart the oscillator at the phase the low band shows
            // right now (the filter has not yet developed its lag).
            let phi = lo.atan2(d / self.omega.max(1e-6));
            let next = 1 - self.cur;
            self.phase[next] = phi.rem_euclid(std::f32::consts::TAU);
            self.cur = next;
            // Fade in over the crossfade (from nothing, or from the tail of
            // the previous hit): the phase is still settling at the onset.
            self.fade = 0.0;
            self.old_env = if self.active { self.prev_env } else { 0.0 };
            self.active = true;
            self.refractory = self.refractory_len;
            self.lock_age = 0;
            self.triggers = (self.triggers + 1) & 1023;
        }
        // Lock window: once the filter has settled, pull the phase onto the
        // input's (measured phase plus the filter's lag).
        if self.lock_age < self.lock_len {
            if self.lock_age >= self.settle_len {
                let meas = lo.atan2(d / self.omega.max(1e-6)) + self.lag;
                let e = (meas - self.phase[self.cur] + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                self.phase[self.cur] += e * LOCK_GAIN;
            }
            self.lock_age += 1;
        }
        self.slow += (self.fast - self.slow) * self.slow_coeff;
        self.prev_env = env;
        for ph in &mut self.phase {
            *ph += self.omega;
            if *ph >= std::f32::consts::TAU {
                *ph -= std::f32::consts::TAU;
            }
        }
        let new = self.phase[self.cur].sin() * env;
        let out = if self.fade < 1.0 {
            self.fade = (self.fade + self.fade_step).min(1.0);
            new * self.fade + self.phase[1 - self.cur].sin() * self.old_env * (1.0 - self.fade)
        } else {
            new
        };
        if env < 1e-4 {
            self.active = false;
        }
        if self.active { (out, env, a) } else { (0.0, 0.0, a) }
    }
}

pub struct DrumBus {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    trim: Smoother,
    // Drive.
    drive_g: Smoother,
    drive_c: Smoother,
    drive_wet: Smoother,
    drive_adaa: [Adaa; 2],
    os: [Oversampler; 2],
    // Crunch.
    crunch_hp: SvfCoeffs,
    crunch_st: [SvfState; 2],
    crunch_k: Smoother,
    crunch_wet: Smoother,
    crunch_adaa: [Adaa; 2],
    crunch_h1: [f32; 2],
    // Compress.
    bus: BusGain,
    comp_thr: f32,
    comp_on: bool,
    comp_amount: Smoother,
    comp_makeup: Smoother,
    // Transients.
    trans_fast: EnvelopeFollower,
    trans_slow: EnvelopeFollower,
    trans_amount: f32,
    trans_gain: Smoother,
    // Boom.
    boom: Boom,
    boom_on: bool,
    boom_amount: Smoother,
    // Damp.
    /// Fades the always-running low-pass in and out (20 ms).
    damp_wet: Smoother,
    damp: SvfCoeffs,
    damp_st: [SvfState; 2],
    mix: Smoother,
    output: Smoother,
    dry: [DelayLine; 2],
    latency: usize,
    // Meters.
    gr_disp: f32,
    in_peak: f32,
    out_peak: f32,
    boom_level: f32,
    trans_db: f32,
    hist_boom: [f32; HISTORY],
    hist_key: [f32; HISTORY],
    bucket_left: u32,
    bucket_len: u32,
    bucket_boom: f32,
    bucket_key: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(DrumBus::new())
}

impl Default for DrumBus {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumBus {
    pub fn new() -> Self {
        let params = params();
        let mut d = Self {
            info: PluginInfo {
                id: ID,
                name: "Drum Bus",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["drums", "bus", "drive", "compressor", "transient", "boom", "sub"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            trim: Smoother::new(1.0),
            drive_g: Smoother::new(1.0),
            drive_c: Smoother::new(1.0),
            drive_wet: Smoother::new(0.0),
            drive_adaa: [Adaa::new(Curve::Tanh, 0.0); 2],
            os: [Oversampler::new(2); 2],
            crunch_hp: SvfCoeffs::passthrough(),
            crunch_st: [SvfState::default(); 2],
            crunch_k: Smoother::new(1.0),
            crunch_wet: Smoother::new(0.0),
            crunch_adaa: [Adaa::new(Curve::Sine, 0.0); 2],
            crunch_h1: [0.0; 2],
            bus: BusGain::new(),
            comp_thr: -10.0,
            comp_on: false,
            comp_amount: Smoother::new(0.0),
            comp_makeup: Smoother::new(1.0),
            trans_fast: EnvelopeFollower::new(DetectorMode::Peak),
            trans_slow: EnvelopeFollower::new(DetectorMode::Peak),
            trans_amount: 0.0,
            trans_gain: Smoother::new(0.0),
            boom: Boom::new(),
            boom_on: false,
            boom_amount: Smoother::new(0.0),
            damp_wet: Smoother::new(0.0),
            damp: SvfCoeffs::passthrough(),
            damp_st: [SvfState::default(); 2],
            mix: Smoother::new(1.0),
            output: Smoother::new(1.0),
            dry: [DelayLine::new(1), DelayLine::new(1)],
            latency: 0,
            gr_disp: 0.0,
            in_peak: 0.0,
            out_peak: 0.0,
            boom_level: 0.0,
            trans_db: 0.0,
            hist_boom: [0.0; HISTORY],
            hist_key: [0.0; HISTORY],
            bucket_left: 480,
            bucket_len: 480,
            bucket_boom: 0.0,
            bucket_key: 0.0,
            first: true,
        };
        d.prepare(48_000.0, 512);
        d
    }

    fn update(&mut self) {
        let ps = &self.params;
        let sr = self.sr;
        self.trim.set_target(db_to_gain(ps.plain(p::TRIM)));
        // Drive.
        let amt = ps.plain(p::DRIVE) / 100.0;
        let curve = drive_curve(ps.choice(p::DRIVE_TYPE));
        let g = db_to_gain(DRIVE_MAX_DB * amt);
        self.drive_g.set_target(g);
        self.drive_c.set_target(drive_makeup(curve, g));
        self.drive_wet.set_target(if ps.on(p::DRIVE_ON) && amt > 0.0 { 1.0 } else { 0.0 });
        for a in &mut self.drive_adaa {
            a.set(curve, 0.0);
        }
        // Crunch.
        let c = ps.plain(p::CRUNCH) / 100.0;
        self.crunch_k.set_target(1.0 + 8.0 * c);
        self.crunch_wet.set_target(if ps.on(p::CRUNCH_ON) && c > 0.0 { 1.0 } else { 0.0 });
        // Compress.
        let c = ps.plain(p::COMPRESS) / 100.0;
        self.comp_on = ps.on(p::COMP_ON) && c > 0.0;
        self.comp_thr = -2.0 - 34.0 * c;
        self.comp_amount.set_target(if self.comp_on { 1.0 } else { 0.0 });
        self.comp_makeup.set_target(db_to_gain(if self.comp_on { 13.6 * c } else { 0.0 }));
        // Transients.
        let t = ps.plain(p::TRANSIENTS) / 100.0;
        self.trans_amount = if ps.on(p::TRANS_ON) { t } else { 0.0 };
        // Boom.
        self.boom_on = ps.on(p::BOOM_ON) && ps.plain(p::BOOM_AMOUNT) > 0.0;
        self.boom_amount.set_target(if self.boom_on { ps.plain(p::BOOM_AMOUNT) / 100.0 } else { 0.0 });
        self.boom.set(ps.plain(p::BOOM_FREQ), ps.plain(p::BOOM_DECAY), sr);
        // Damp.
        let hz = ps.plain(p::DAMP);
        self.damp_wet.set_target(if ps.on(p::DAMP_ON) && hz < 19_500.0 { 1.0 } else { 0.0 });
        self.damp = SvfCoeffs::new(SvfMode::Lowpass, hz, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
        self.mix.set_target(ps.plain(p::MIX) / 100.0);
        self.output.set_target(db_to_gain(ps.plain(p::OUTPUT)));
        if self.first {
            for s in [
                &mut self.trim,
                &mut self.drive_g,
                &mut self.drive_c,
                &mut self.drive_wet,
                &mut self.crunch_k,
                &mut self.crunch_wet,
                &mut self.comp_amount,
                &mut self.comp_makeup,
                &mut self.boom_amount,
                &mut self.damp_wet,
                &mut self.mix,
                &mut self.output,
            ] {
                s.snap_to_target();
            }
            self.first = false;
        }
    }

    fn end_bucket(&mut self) {
        self.hist_boom.rotate_left(1);
        self.hist_key.rotate_left(1);
        self.hist_boom[HISTORY - 1] = self.bucket_boom;
        self.hist_key[HISTORY - 1] = self.bucket_key;
        self.bucket_boom = 0.0;
        self.bucket_key = 0.0;
        self.bucket_left = self.bucket_len;
    }
}

impl Segmented for DrumBus {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let lat = self.latency;
        let comp_on = self.comp_on;
        let (thr, trans_amount) = (self.comp_thr, self.trans_amount);
        let mut in_peak = self.in_peak * 0.5;
        let mut out_peak = self.out_peak * 0.5;
        let mut gr_max = 0.0f32;
        let this = &mut *self;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            in_peak = in_peak.max(l.abs()).max(r.abs());
            this.dry[0].push(l);
            this.dry[1].push(r);
            let dry = [this.dry[0].read(lat), this.dry[1].read(lat)];
            let trim = this.trim.next();
            let x = [l * trim, r * trim];

            // Drive on the 2× oversampler (always in the path: constant latency).
            let (g, c, w) = (this.drive_g.next(), this.drive_c.next(), this.drive_wet.next());
            let mut y = [0.0f32; 2];
            for ch in 0..2 {
                let mut up = [0.0f32; 4];
                this.os[ch].upsample(x[ch], &mut up);
                if w > 1e-5 {
                    for v in up.iter_mut().take(2) {
                        let s = this.drive_adaa[ch].process(*v * g) * c;
                        *v += (s - *v) * w;
                    }
                }
                y[ch] = this.os[ch].downsample(&up);
            }

            // Crunch: add only the distortion of the band above 500 Hz.
            let (k, cw) = (this.crunch_k.next(), this.crunch_wet.next());
            if cw > 1e-5 {
                for ch in 0..2 {
                    let h = this.crunch_st[ch].process(&this.crunch_hp, y[ch]);
                    let shaped = this.crunch_adaa[ch].process(h * k) / k;
                    // ADAA1 of the identity is the mean of the last two inputs.
                    let linear = 0.5 * (h + this.crunch_h1[ch]);
                    this.crunch_h1[ch] = h;
                    y[ch] += (shaped - linear) * cw;
                }
            }

            // Compress: fixed 4:1, 10 ms attack, automatic release.
            let ca = this.comp_amount.next();
            let mk = this.comp_makeup.next();
            if comp_on || ca > 1e-5 {
                let gr = this.bus.process(y[0], y[1], thr, 4.0, 40.0) * ca;
                gr_max = gr_max.max(gr);
                let gain = db_to_gain(-gr) * mk;
                y[0] *= gain;
                y[1] *= gain;
            }

            // Transients: level difference of a fast and a slow envelope.
            let key = y[0].abs().max(y[1].abs());
            let fast = gain_to_db(this.trans_fast.process(key));
            let slow = gain_to_db(this.trans_slow.process(key));
            let att = if fast > -70.0 { (fast - slow).clamp(0.0, 24.0) } else { 0.0 };
            this.trans_gain.set_target((trans_amount * att * 0.6).clamp(-12.0, 12.0));
            let tg = this.trans_gain.next();
            this.trans_db = tg;
            if tg.abs() > 1e-3 {
                let gain = db_to_gain(tg);
                y[0] *= gain;
                y[1] *= gain;
            }

            // Boom: sine locked to the hits of the low band (key aligned
            // with the wet path by the dry delay).
            let amount = this.boom_amount.next();
            if this.boom_on || amount > 1e-5 {
                let m = 0.5 * (dry[0] + dry[1]) * trim;
                let (osc, env, key_abs) = this.boom.process(m);
                let b = osc * amount;
                y[0] += b;
                y[1] += b;
                this.boom_level = env * amount;
                this.bucket_boom = this.bucket_boom.max(b.abs());
                this.bucket_key = this.bucket_key.max(key_abs);
            } else {
                this.boom_level = 0.0;
            }
            if this.bucket_left == 0 {
                this.end_bucket();
            } else {
                this.bucket_left -= 1;
            }

            // Damp.
            // The low-pass always runs so its state is current when it fades in.
            let dw = this.damp_wet.next();
            for ch in 0..2 {
                let lp = this.damp_st[ch].process(&this.damp, y[ch]);
                y[ch] += (lp - y[ch]) * dw;
            }

            let mix = this.mix.next();
            let og = this.output.next();
            let ol = (dry[0] + (y[0] - dry[0]) * mix) * og;
            let or = (dry[1] + (y[1] - dry[1]) * mix) * og;
            out_peak = out_peak.max(ol.abs()).max(or.abs());
            (sanitize(ol), sanitize(or))
        });
        self.in_peak = in_peak;
        self.out_peak = out_peak;
        let fall = GR_FALL_DB_S * (end - start) as f32 / self.sr as f32;
        self.gr_disp = gr_max.max(self.gr_disp - fall).max(0.0);
    }
}

impl Processor for DrumBus {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.trim, &mut self.drive_g, &mut self.drive_c, &mut self.drive_wet, &mut self.crunch_k, &mut self.crunch_wet, &mut self.comp_amount, &mut self.comp_makeup, &mut self.boom_amount, &mut self.damp_wet, &mut self.mix, &mut self.output] {
            s.set_time(20.0, sample_rate);
        }
        self.trans_gain.set_time(1.0, sample_rate);
        self.os = [Oversampler::new(2); 2];
        // Round trip of the 2× stage plus half a high-rate sample of ADAA.
        self.latency = (Oversampler::new(2).latency() + 0.25).round() as usize;
        self.dry = [DelayLine::new(64), DelayLine::new(64)];
        self.crunch_hp = SvfCoeffs::new(SvfMode::Highpass, CRUNCH_HZ, std::f32::consts::FRAC_1_SQRT_2, 0.0, sample_rate);
        self.bus.prepare(sample_rate);
        self.bus.set_attack_ms(10.0, sample_rate);
        self.bus.set_release(None, sample_rate);
        self.trans_fast.set_times(0.3, 25.0, sample_rate);
        self.trans_slow.set_times(15.0, 25.0, sample_rate);
        self.boom.prepare(sample_rate);
        self.bucket_len = ((BUCKET_MS as f64 * 0.001 * sample_rate) as u32).max(1);
        self.bucket_left = self.bucket_len;
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for o in &mut self.os {
            o.reset();
        }
        for a in self.drive_adaa.iter_mut().chain(&mut self.crunch_adaa) {
            a.reset();
        }
        for s in self.crunch_st.iter_mut().chain(&mut self.damp_st) {
            s.reset();
        }
        for d in &mut self.dry {
            d.clear();
        }
        self.crunch_h1 = [0.0; 2];
        self.bus.reset();
        self.trans_fast.reset();
        self.trans_slow.reset();
        self.trans_gain.snap(0.0);
        self.boom.reset();
        self.gr_disp = 0.0;
        self.in_peak = 0.0;
        self.out_peak = 0.0;
        self.boom_level = 0.0;
        self.trans_db = 0.0;
        self.hist_boom = [0.0; HISTORY];
        self.hist_key = [0.0; HISTORY];
        self.bucket_boom = 0.0;
        self.bucket_key = 0.0;
        self.bucket_left = self.bucket_len;
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
        let head = [-self.gr_disp, gain_to_db(self.in_peak), gain_to_db(self.out_peak), self.latency as f32, self.boom_level, self.boom.triggers as f32, self.trans_db];
        let mut o = write_meter(out, 0, &head);
        o = write_meter(out, o, &self.hist_boom);
        write_meter(out, o, &self.hist_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, rms, set_plain};

    /// Turn every stage off except what a test enables: the output is then
    /// the input delayed by the latency (plus whatever is enabled).
    fn neutral() -> DrumBus {
        let mut d = DrumBus::new();
        set_plain(&mut d, p::DRIVE, 0.0);
        set_plain(&mut d, p::CRUNCH, 0.0);
        set_plain(&mut d, p::COMPRESS, 0.0);
        set_plain(&mut d, p::TRANSIENTS, 0.0);
        set_plain(&mut d, p::BOOM_AMOUNT, 0.0);
        set_plain(&mut d, p::DAMP, 20_000.0);
        d
    }

    /// Decaying sine hits `(start sample, amplitude, start phase)`.
    fn kicks(freq: f32, tau_ms: f32, hits: &[(usize, f32, f32)]) -> impl Fn(usize) -> (f32, f32) + '_ {
        move |i| {
            let mut v = 0.0f64;
            for &(s, a, ph) in hits {
                if i >= s {
                    let t = (i - s) as f64 / SR;
                    v += a as f64 * (-t / (tau_ms as f64 * 0.001)).exp() * (std::f64::consts::TAU * freq as f64 * t + ph as f64).sin();
                }
            }
            (v as f32, v as f32)
        }
    }

    fn corr(a: &[f32], b: &[f32]) -> f32 {
        let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
        for (x, y) in a.iter().zip(b) {
            ab += (*x as f64) * (*y as f64);
            aa += (*x as f64) * (*x as f64);
            bb += (*y as f64) * (*y as f64);
        }
        (ab / (aa * bb).sqrt().max(1e-30)) as f32
    }

    /// The Boom signal alone: output with the Boom minus output without it.
    fn boom_only(setup: impl Fn(&mut DrumBus), input: impl Fn(usize) -> (f32, f32) + Copy, n: usize) -> Vec<f32> {
        let mut a = neutral();
        setup(&mut a);
        set_plain(&mut a, p::BOOM_AMOUNT, 100.0);
        let mut b = neutral();
        setup(&mut b);
        let (with, _) = process_stereo(&mut a, input, n);
        let (without, _) = process_stereo(&mut b, input, n);
        with.iter().zip(&without).map(|(x, y)| x - y).collect()
    }

    #[test]
    fn registered_with_a_constant_latency() {
        let p = crate::create(ID).unwrap();
        assert_eq!(p.info().id, ID);
        assert!(p.latency() > 0 && p.latency() < 32);
    }

    #[test]
    fn neutral_settings_pass_the_signal_delayed() {
        let mut d = neutral();
        let lat = d.latency() as usize;
        let input = |i: usize| {
            let v = 0.3 * (std::f64::consts::TAU * 440.0 * i as f64 / SR).sin() as f32;
            (v, v)
        };
        let (l, _) = process_stereo(&mut d, input, 24_000);
        for i in 2_000..24_000 {
            // The half-band's delay is not a whole number of samples.
            assert!((l[i] - input(i - lat).0).abs() < 0.02, "sample {i}: {} vs {}", l[i], input(i - lat).0);
        }
        // Mix 0 is exactly the aligned dry signal, whatever the stages do.
        let mut d = DrumBus::new();
        set_plain(&mut d, p::DRIVE, 100.0);
        set_plain(&mut d, p::CRUNCH, 100.0);
        set_plain(&mut d, p::BOOM_AMOUNT, 100.0);
        set_plain(&mut d, p::MIX, 0.0);
        let (l, _) = process_stereo(&mut d, input, 24_000);
        for i in 2_000..24_000 {
            assert!((l[i] - input(i - lat).0).abs() < 1e-5, "mix 0 sample {i}");
        }
    }

    #[test]
    fn boom_follows_the_low_band_envelope_and_locks_to_the_hit() {
        let boom_hz = 60.0;
        let setup = |d: &mut DrumBus| {
            set_plain(d, p::BOOM_FREQ, boom_hz);
            set_plain(d, p::BOOM_DECAY, 300.0);
        };
        // Hits at awkward positions in the bar, with both polarities and
        // several levels: a free-running oscillator would cancel on some.
        let pi = std::f32::consts::PI;
        let hits = [(9_000usize, 0.8f32, 0.0f32), (40_321, 0.8, pi), (72_777, 0.4, 0.0), (105_011, 0.2, pi), (137_555, 0.8, 0.5 * pi), (170_000, 0.8, 1.5 * pi)];
        let input = kicks(boom_hz, 90.0, &hits);
        let n = 205_000;
        let boom = boom_only(setup, &input, n);
        let key: Vec<f32> = (0..n).map(|i| input(i).0).collect();
        let lat = DrumBus::new().latency() as usize;
        let mut rms_per_hit = Vec::new();
        let mut worst = 1.0f32;
        for &(s, a, _) in &hits {
            // 40 ms after the hit to 160 ms: past the onset and its filter.
            let (w0, w1) = (s + 1_920, s + 7_680);
            let c = corr(&boom[w0..w1], &key[w0 - lat..w1 - lat]);
            assert!(c > 0.9, "hit at {s} (level {a}): boom/kick correlation {c}");
            worst = worst.min(c);
            rms_per_hit.push(rms(&boom[w0..w1]));
        }
        // Level tracks the hit: 0.8 : 0.4 : 0.2.
        let r = |k: usize| rms_per_hit[k] / rms_per_hit[0];
        assert!((r(2) - 0.5).abs() < 0.1, "0.4 hit: {}", r(2));
        assert!((r(3) - 0.25).abs() < 0.07, "0.2 hit: {}", r(3));
        eprintln!("boom/kick correlation: worst of {} hits {worst:.3}; level ratios {:.2} {:.2}", hits.len(), r(2), r(3));
    }

    #[test]
    fn boom_ignores_content_above_the_low_band() {
        let setup = |_: &mut DrumBus| {};
        let burst = |hz: f32| {
            move |i: usize| {
                let v = if (9_000..14_000).contains(&i) { 0.8 * (std::f64::consts::TAU * hz as f64 * i as f64 / SR).sin() as f32 } else { 0.0 };
                (v, v)
            }
        };
        let low = peak(&boom_only(setup, burst(60.0), 40_000));
        for hz in [1_000.0f32, 5_000.0] {
            let high = peak(&boom_only(setup, burst(hz), 40_000));
            assert!(high < low * 0.05, "{hz} Hz burst made {high} of boom against {low}");
        }
        // Silence stays silent.
        assert_eq!(peak(&boom_only(setup, |_| (0.0, 0.0), 20_000)), 0.0);
    }

    #[test]
    fn boom_oscillates_at_its_frequency_and_decay_sets_the_tail() {
        let hits = [(9_000usize, 0.8f32, 0.0f32)];
        let input = kicks(60.0, 60.0, &hits);
        let n = 60_000;
        // Zero crossings between 100 and 300 ms after the hit.
        let crossings = |hz: f32| {
            let b = boom_only(|d: &mut DrumBus| set_plain(d, p::BOOM_FREQ, hz), &input, n);
            let w = &b[9_000 + 4_800..9_000 + 14_400];
            let c = w.windows(2).filter(|p| p[0] < 0.0 && p[1] >= 0.0).count();
            c as f32 / 0.2
        };
        for hz in [40.0f32, 70.0, 110.0] {
            let f = crossings(hz);
            assert!((f - hz).abs() < hz * 0.12 + 5.0, "asked {hz} Hz, got {f}");
        }
        let tail = |decay: f32| {
            let b = boom_only(|d: &mut DrumBus| set_plain(d, p::BOOM_DECAY, decay), &input, n);
            rms(&b[9_000 + 14_400..9_000 + 19_200]) // 300-400 ms
        };
        let (short, long) = (tail(50.0), tail(800.0));
        assert!(long > 20.0 * short.max(1e-9), "decay 800 ms tail {long} vs 50 ms {short}");
    }

    #[test]
    fn overlapping_hits_crossfade_without_clicks() {
        // Kicks every 90 ms with a long boom tail: the oscillator restarts
        // under a live tail each time.
        let hits: Vec<(usize, f32, f32)> = (0..8).map(|k| (5_000 + k * 4_321, 0.7, 0.0)).collect();
        let input = kicks(55.0, 120.0, &hits);
        let b = boom_only(|d: &mut DrumBus| set_plain(d, p::BOOM_DECAY, 800.0), &input, 50_000);
        let worst = b.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        // A 55 Hz sine of this level moves at most ω·A ≈ 0.007 per sample.
        assert!(worst < 0.02, "largest step {worst}");
    }

    #[test]
    fn drive_is_level_matched_and_adds_harmonics() {
        let tone = |amp: f32| {
            move |i: usize| {
                let v = amp * (std::f64::consts::TAU * 1_000.0 * i as f64 / SR).sin() as f32;
                (v, v)
            }
        };
        for ty in 0..3 {
            let mut d = neutral();
            set_plain(&mut d, p::DRIVE, 100.0);
            set_plain(&mut d, p::DRIVE_TYPE, ty as f32);
            let (l, _) = process_stereo(&mut d, tone(0.5), 24_000);
            let pk = peak(&l[12_000..]);
            assert!(pk > 0.3 && pk < 0.7, "type {ty}: −6 dBFS tone comes out at {pk}");
        }
        // A quiet tone gains harmonics at full drive: the shape is no longer a sine.
        let mut d = neutral();
        set_plain(&mut d, p::DRIVE, 100.0);
        set_plain(&mut d, p::DRIVE_TYPE, drive_type::HARD);
        let (l, _) = process_stereo(&mut d, tone(0.5), 24_000);
        let flat = l[12_000..].iter().filter(|v| v.abs() > 0.45).count();
        assert!(flat > 2_000, "hard drive did not flatten the peaks ({flat})");
    }

    #[test]
    fn crunch_leaves_quiet_highs_alone_and_distorts_loud_ones() {
        let tone = |amp: f32| {
            move |i: usize| {
                let v = amp * (std::f64::consts::TAU * 3_000.0 * i as f64 / SR).sin() as f32;
                (v, v)
            }
        };
        let gain = |amp: f32, crunch: f32| {
            let mut d = neutral();
            set_plain(&mut d, p::CRUNCH, crunch);
            let (l, _) = process_stereo(&mut d, tone(amp), 24_000);
            rms(&l[12_000..]) / (amp / 2f32.sqrt())
        };
        assert!((gain(0.01, 100.0) - 1.0).abs() < 0.02, "quiet: {}", gain(0.01, 100.0));
        assert!(gain(0.8, 100.0) < 0.8 * gain(0.8, 0.0), "loud highs are shaped");
        // Low frequencies are outside the band.
        let mut d = neutral();
        set_plain(&mut d, p::CRUNCH, 100.0);
        let low = |i: usize| {
            let v = 0.8 * (std::f64::consts::TAU * 80.0 * i as f64 / SR).sin() as f32;
            (v, v)
        };
        let (l, _) = process_stereo(&mut d, low, 24_000);
        assert!((rms(&l[12_000..]) / (0.8 / 2f32.sqrt()) - 1.0).abs() < 0.08);
    }

    #[test]
    fn compress_reduces_loud_hits_and_reports_it() {
        let burst = |i: usize| {
            let a = if i < 24_000 { 0.7 } else { 0.0 };
            let v = a * (std::f64::consts::TAU * 1_000.0 * i as f64 / SR).sin() as f32;
            (v, v)
        };
        let mut d = neutral();
        set_plain(&mut d, p::COMPRESS, 60.0);
        process_stereo(&mut d, burst, 24_000);
        let mut m = [0.0f32; 160];
        assert_eq!(d.meter(&mut m), 135);
        assert!(m[0] < -6.0, "reported reduction {}", m[0]);
        assert!(m.iter().all(|v| v.is_finite()));
        // Same signal with the stage switched off is not reduced.
        let mut d = neutral();
        set_plain(&mut d, p::COMPRESS, 60.0);
        set_plain(&mut d, p::COMP_ON, 0.0);
        let (l, _) = process_stereo(&mut d, burst, 24_000);
        assert!((peak(&l[12_000..]) - 0.7).abs() < 0.02);
    }

    #[test]
    fn transients_push_and_soften_onsets() {
        // A hit with a sharp onset and a 150 ms tail.
        let hit = |i: usize| {
            let t = (i % 24_000) as f64 / SR;
            let v = 0.5 * (-t / 0.06).exp() * (std::f64::consts::TAU * 800.0 * t).sin();
            (v as f32, v as f32)
        };
        let onset_peak = |trans: f32| {
            let mut d = neutral();
            set_plain(&mut d, p::TRANSIENTS, trans);
            let (l, _) = process_stereo(&mut d, hit, 72_000);
            peak(&l[48_000..48_000 + 480])
        };
        let (plus, zero, minus) = (onset_peak(100.0), onset_peak(0.0), onset_peak(-100.0));
        assert!(plus > zero * 1.3, "{plus} vs {zero}");
        assert!(minus < zero * 0.8, "{minus} vs {zero}");
    }

    #[test]
    fn damp_rolls_off_the_highs() {
        let tone = |hz: f32| {
            move |i: usize| {
                let v = 0.3 * (std::f64::consts::TAU * hz as f64 * i as f64 / SR).sin() as f32;
                (v, v)
            }
        };
        let level = |damp: f32, hz: f32, on: f32| {
            let mut d = neutral();
            set_plain(&mut d, p::DAMP, damp);
            set_plain(&mut d, p::DAMP_ON, on);
            let (l, _) = process_stereo(&mut d, tone(hz), 24_000);
            peak(&l[12_000..])
        };
        assert!(level(2_000.0, 10_000.0, 1.0) < 0.3 * 0.1);
        assert!((level(2_000.0, 200.0, 1.0) - 0.3).abs() < 0.02);
        assert!((level(2_000.0, 10_000.0, 0.0) - 0.3).abs() < 0.02, "switch off");
    }

    #[test]
    fn meters_show_the_boom() {
        let hits = [(9_000usize, 0.8f32, 0.0f32)];
        let mut d = neutral();
        set_plain(&mut d, p::BOOM_AMOUNT, 100.0);
        process_stereo(&mut d, kicks(55.0, 90.0, &hits), 24_000);
        let mut m = [0.0f32; 160];
        let n = d.meter(&mut m);
        assert_eq!(n, 135);
        assert!(m[5] >= 1.0, "trigger counter {}", m[5]);
        let hist_boom = &m[7..71];
        let hist_key = &m[71..135];
        assert!(hist_boom.iter().cloned().fold(0.0, f32::max) > 0.1);
        assert!(hist_key.iter().cloned().fold(0.0, f32::max) > 0.1);
        assert!(n <= 320, "meter() must fit the engine's 320-value cap");
    }

    #[test]
    fn factory_presets_are_complete() {
        let list = crate::presets::list(ID);
        assert!(list.len() >= 8, "{} presets", list.len());
        for r in list {
            let preset = crate::presets::load(&r.path).unwrap();
            let mut d = DrumBus::new();
            assert_eq!(preset.apply(&mut d), params().len(), "{}", r.name);
        }
    }

    #[test]
    fn state_round_trips() {
        let mut a = DrumBus::new();
        set_plain(&mut a, p::BOOM_AMOUNT, 70.0);
        set_plain(&mut a, p::DRIVE_TYPE, 2.0);
        let mut b = DrumBus::new();
        b.load_state(&a.save_state());
        for info in params() {
            assert!((a.get_param(info.id) - b.get_param(info.id)).abs() < 1e-5, "{}", info.name);
        }
    }

    /// Every stepped parameter and the Damp frequency change by an event in
    /// mid-block while a 100 Hz sine plays: the largest step between
    /// contiguous samples must stay of the order of the clean signal's own.
    #[test]
    fn discrete_changes_do_not_click() {
        use crate::test_util::param_change_step;
        let setup = [
            (p::DRIVE, 100.0),
            (p::CRUNCH, 60.0),
            (p::COMPRESS, 60.0),
            (p::TRANSIENTS, 60.0),
            (p::BOOM_AMOUNT, 60.0),
            (p::BOOM_FREQ, 100.0),
            (p::DAMP, 2_000.0),
        ];
        let mut cases: Vec<(u32, &str, f32, f32)> = vec![
            (p::DRIVE_TYPE, "type soft>hard", 0.0, 2.0),
            (p::DRIVE_TYPE, "type hard>soft", 2.0, 0.0),
            (p::DRIVE_TYPE, "type med>soft", 1.0, 0.0),
            (p::DAMP, "damp 20k>1k", 20_000.0, 1_000.0),
            (p::DAMP, "damp 1k>20k", 1_000.0, 20_000.0),
        ];
        for (id, name) in [(p::DRIVE_ON, "drive"), (p::CRUNCH_ON, "crunch"), (p::COMP_ON, "comp"), (p::TRANS_ON, "trans"), (p::BOOM_ON, "boom"), (p::DAMP_ON, "damp")] {
            cases.push((id, if name == "drive" { "drive off" } else { name }, 1.0, 0.0));
            cases.push((id, if name == "drive" { "drive on" } else { name }, 0.0, 1.0));
        }
        let mut bad = Vec::new();
        for (id, name, from, to) in cases {
            let (ev, reference) = param_change_step(create, &setup, id, from, to, 0.8);
            eprintln!("{name:<16} {from:>7} > {to:<7} step {ev:.4} (clean {reference:.4})");
            if ev > 2.0 * reference + 0.005 {
                bad.push(format!("{name}: step {ev:.4} against {reference:.4}"));
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }
}
