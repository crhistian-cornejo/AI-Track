//! Poly "analog" synth: two oscillators per voice (A can hard-sync to B),
//! poly-mod (filter envelope and oscillator B → A's frequency, A's pulse
//! width and the cutoff), a 24 dB OTA-style low-pass, slight per-voice
//! drift of pitch and cutoff, and a built-in stereo chorus (I, II, I+II).

use super::Engine;
use super::kit::{Cascade4, Drift, Ensemble, FilterModel, Perf, Pool, Rng, Voice, coeff, flush, key_hz, sin01};
use super::params::{Params, poly as pp};
use aitrack_dsp::envelope::Adsr;
use aitrack_dsp::osc::poly_blep;

pub const VOICES: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
pub struct Settings {
    sr: f32,
    a_wave: usize,
    a_pw: f32,
    b_wave: usize,
    b_pw: f32,
    b_ratio: f32,
    mix: f32,
    noise: f32,
    sync: bool,
    cutoff: f32,
    reso: f32,
    env: f32,
    key_track: f32,
    pm_env: f32,
    pm_b: f32,
    pm_freq: bool,
    pm_pw: bool,
    pm_filter: bool,
    lfo_rate: f32,
    vibrato: f32,
    lfo_pw: f32,
    lfo_cutoff: f32,
    drift: f32,
    glide_ms: f32,
    vel_amp: f32,
    vel_filter: f32,
    spread: f32,
    fenv: [f32; 4],
    aenv: [f32; 4],
}

/// Band-limited saw / pulse / triangle / sine from a phase and increment.
#[inline]
fn wave(kind: usize, t: f32, dt: f32, pw: f32) -> f32 {
    match kind {
        0 => 2.0 * t - 1.0 - poly_blep(t, dt),
        1 => {
            let mut y = if t < pw { 1.0 } else { -1.0 };
            y += poly_blep(t, dt);
            y -= poly_blep((t - pw).rem_euclid(1.0), dt);
            y - (2.0 * pw - 1.0)
        }
        2 => 1.0 - 4.0 * (t - 0.5).abs(),
        _ => sin01(t),
    }
}

pub struct PolyVoice {
    pa: f32,
    pb: f32,
    dta: f32,
    dtb: f32,
    pw: f32,
    filter: Cascade4,
    fenv: Adsr,
    aenv: Adsr,
    key: f32,
    target: f32,
    vel: f32,
    lfo: f32,
    drift: Drift,
    drift_f: Drift,
    rng: Rng,
    pan: (f32, f32),
    last_b: f32,
    cut_base: f32,
    seed: u32,
}

impl PolyVoice {
    fn new(sr: f64, seed: u32) -> Self {
        Self {
            pa: 0.0,
            pb: 0.0,
            dta: 0.0,
            dtb: 0.0,
            pw: 0.5,
            filter: Cascade4::new(FilterModel::Ota),
            fenv: Adsr::new(sr),
            aenv: Adsr::new(sr),
            key: 60.0,
            target: 60.0,
            vel: 1.0,
            lfo: (seed % 97) as f32 / 97.0,
            drift: Drift::new(seed.wrapping_mul(2_654_435_761)),
            drift_f: Drift::new(seed.wrapping_mul(40_503).wrapping_add(7)),
            rng: Rng::new(seed.wrapping_mul(747_796_405).wrapping_add(1)),
            pan: (0.707, 0.707),
            last_b: 0.0,
            cut_base: 1000.0,
            seed,
        }
    }
}

impl Voice for PolyVoice {
    type Settings = Settings;

    fn start(&mut self, key: u8, vel: f32, retrigger: bool, s: &Settings, _perf: &Perf) {
        self.target = key as f32;
        if s.glide_ms <= 0.0 || !self.aenv.is_active() {
            self.key = self.target;
        }
        self.vel = vel;
        if !retrigger {
            self.filter.reset();
            // Free-running oscillators: a random start phase per note.
            self.pa = 0.5 * (self.rng.next() + 1.0);
            self.pb = 0.5 * (self.rng.next() + 1.0);
        }
        self.fenv.set_sample_rate(s.sr as f64);
        self.aenv.set_sample_rate(s.sr as f64);
        self.fenv.set(s.fenv[0], s.fenv[1], s.fenv[2], s.fenv[3]);
        self.aenv.set(s.aenv[0], s.aenv[1], s.aenv[2], s.aenv[3]);
        self.fenv.gate_on();
        self.aenv.gate_on();
        // Alternate the voices across the stereo field.
        let side = if self.seed.is_multiple_of(2) { 1.0 } else { -1.0 } * ((self.seed / 2) % 4 + 1) as f32 / 4.0;
        let a = (side * s.spread + 1.0) * std::f32::consts::FRAC_PI_4;
        self.pan = (a.cos(), a.sin());
    }

    fn release(&mut self, s: &Settings) {
        self.fenv.set(s.fenv[0], s.fenv[1], s.fenv[2], s.fenv[3]);
        self.aenv.set(s.aenv[0], s.aenv[1], s.aenv[2], s.aenv[3]);
        self.fenv.gate_off();
        self.aenv.gate_off();
    }

    fn active(&self) -> bool {
        self.aenv.is_active()
    }

    fn control(&mut self, s: &Settings, perf: &Perf) {
        let n = super::kit::CONTROL as f32;
        if s.glide_ms > 0.0 {
            let c = 1.0 - (1.0 - coeff(s.glide_ms / 3.0, s.sr)).powf(n);
            self.key += (self.target - self.key) * c;
        } else {
            self.key = self.target;
        }
        self.lfo += s.lfo_rate * n / s.sr;
        self.lfo -= self.lfo.floor();
        let lfo = sin01(self.lfo);
        let drift = self.drift.next() * s.drift * 0.12;
        let pitch = self.key + perf.bend + lfo * (s.vibrato / 100.0 + perf.mod_wheel * 0.5) + drift;
        let a_hz = key_hz(pitch);
        self.dta = (a_hz / s.sr).min(0.45);
        self.dtb = (a_hz * s.b_ratio / s.sr).min(0.45);
        self.pw = (s.a_pw + s.lfo_pw * 0.4 * lfo).clamp(0.05, 0.95);
        let fe = self.fenv.value();
        let vel = 1.0 - s.vel_filter + s.vel_filter * self.vel;
        let oct = s.env * 5.0 * fe * vel + s.key_track * (self.key - 60.0) / 12.0 + s.lfo_cutoff * 2.0 * lfo + self.drift_f.next() * s.drift * 0.15;
        self.cut_base = s.cutoff * 2f32.powf(oct);
        self.filter.set(self.cut_base, s.reso, s.sr);
    }

    #[inline]
    fn tick(&mut self, s: &Settings) -> (f32, f32) {
        // Oscillator B first: it is a poly-mod source.
        let b = wave(s.b_wave, self.pb, self.dtb, s.b_pw);
        let fe = self.fenv.next();
        let pm = s.pm_env * fe + s.pm_b * b;
        let mut dta = self.dta;
        if s.pm_freq && pm != 0.0 {
            dta = (dta * 2f32.powf(pm * 3.0)).clamp(0.0, 0.45);
        }
        let pw = if s.pm_pw { (self.pw + 0.45 * pm).clamp(0.05, 0.95) } else { self.pw };
        let a = wave(s.a_wave, self.pa, dta, pw);
        // Advance.
        self.pb += self.dtb;
        let mut wrapped = false;
        if self.pb >= 1.0 {
            self.pb -= 1.0;
            wrapped = true;
        }
        self.pa += dta;
        if s.sync && wrapped {
            self.pa = self.pb * dta / self.dtb.max(1e-6);
        }
        if self.pa >= 1.0 {
            self.pa -= self.pa.floor();
        }
        self.last_b = b;
        let mut x = a * (1.0 - s.mix) + b * s.mix;
        if s.noise > 0.0 {
            x += s.noise * self.rng.next();
        }
        if s.pm_filter && pm != 0.0 {
            self.filter.set(self.cut_base * 2f32.powf(pm * 3.0), s.reso, s.sr);
        }
        let y = self.filter.process(0.5 * x);
        let amp = self.aenv.next() * (1.0 - s.vel_amp + s.vel_amp * self.vel);
        let o = flush(y * amp * 0.8);
        (o * self.pan.0, o * self.pan.1)
    }

    fn kill(&mut self) {
        self.aenv.reset();
        self.fenv.reset();
        self.filter.reset();
    }
}

pub struct Poly {
    s: Settings,
    pool: Pool<PolyVoice>,
    chorus: Ensemble,
    chorus_mode: usize,
    scratch: [Vec<f32>; 2],
}

impl Poly {
    pub fn new(sr: f64) -> Self {
        let voices = (0..VOICES as u32).map(|i| PolyVoice::new(sr, i * 7 + 3)).collect();
        Self { s: Settings { sr: sr as f32, ..Default::default() }, pool: Pool::new(voices), chorus: Ensemble::new(sr), chorus_mode: 0, scratch: [vec![0.0; 4096], vec![0.0; 4096]] }
    }

    pub fn pool(&self) -> &Pool<PolyVoice> {
        &self.pool
    }
}

impl Engine for Poly {
    fn prepare(&mut self, sr: f64) {
        if sr as f32 != self.s.sr {
            *self = Self::new(sr);
        }
    }

    fn reset(&mut self) {
        self.pool.kill_all();
        self.chorus.reset();
    }

    fn update(&mut self, p: &Params) {
        let s = &mut self.s;
        s.a_wave = p.choice(pp::A_WAVE);
        s.a_pw = p.plain(pp::A_PW) / 100.0;
        s.b_wave = p.choice(pp::B_WAVE);
        s.b_pw = p.plain(pp::B_PW) / 100.0;
        s.b_ratio = 2f32.powf((p.plain(pp::B_SEMI) + p.plain(pp::B_FINE) / 100.0) / 12.0);
        s.mix = p.plain(pp::MIX) / 100.0;
        s.noise = p.plain(pp::NOISE) / 100.0;
        s.sync = p.on(pp::SYNC);
        s.cutoff = p.plain(pp::CUTOFF);
        s.reso = p.plain(pp::RESONANCE) / 100.0;
        s.env = p.plain(pp::ENV_AMOUNT) / 100.0;
        s.key_track = p.plain(pp::KEY_TRACK) / 100.0;
        s.pm_env = p.plain(pp::PM_ENV) / 100.0;
        s.pm_b = p.plain(pp::PM_OSC_B) / 100.0;
        s.pm_freq = p.on(pp::PM_FREQ);
        s.pm_pw = p.on(pp::PM_PW);
        s.pm_filter = p.on(pp::PM_FILTER);
        s.lfo_rate = p.plain(pp::LFO_RATE);
        s.vibrato = p.plain(pp::LFO_PITCH);
        s.lfo_pw = p.plain(pp::LFO_PW) / 100.0;
        s.lfo_cutoff = p.plain(pp::LFO_CUTOFF) / 100.0;
        s.drift = p.plain(pp::DRIFT) / 100.0;
        s.glide_ms = p.plain(pp::GLIDE);
        s.vel_amp = p.plain(pp::VEL_AMP) / 100.0;
        s.vel_filter = p.plain(pp::VEL_FILTER) / 100.0;
        s.spread = p.plain(pp::SPREAD) / 100.0;
        s.fenv = [p.plain(pp::F_ATTACK), p.plain(pp::F_DECAY), p.plain(pp::F_SUSTAIN) / 100.0, p.plain(pp::F_RELEASE)];
        s.aenv = [p.plain(pp::A_ATTACK), p.plain(pp::A_DECAY), p.plain(pp::A_SUSTAIN) / 100.0, p.plain(pp::A_RELEASE)];
        // Running envelopes follow knob changes.
        for v in &mut self.pool.voices {
            v.fenv.set(s.fenv[0], s.fenv[1], s.fenv[2], s.fenv[3]);
            v.aenv.set(s.aenv[0], s.aenv[1], s.aenv[2], s.aenv[3]);
        }
        self.chorus_mode = p.choice(pp::CHORUS);
        let c = &mut self.chorus;
        c.taps = 2;
        match self.chorus_mode {
            1 => (c.base_ms, c.depth_ms, c.rate, c.mix) = (6.0, 1.8, 0.5, 0.8),
            2 => (c.base_ms, c.depth_ms, c.rate, c.mix) = (5.0, 1.6, 0.85, 0.85),
            3 => (c.base_ms, c.depth_ms, c.rate, c.mix) = (5.5, 0.5, 8.0, 0.7),
            _ => c.mix = 0.0,
        }
        self.pool.refresh();
    }

    fn note_on(&mut self, key: u8, vel: f32, perf: &Perf) {
        self.pool.note_on(key, vel, &self.s, perf);
    }

    fn note_off(&mut self, key: u8) {
        self.pool.note_off(key, &self.s);
    }

    fn release_all(&mut self) {
        self.pool.release_all(&self.s);
    }

    fn render(&mut self, perf: &Perf, l: &mut [f32], r: &mut [f32]) {
        let n = l.len().min(self.scratch[0].len());
        let [sl, sr] = &mut self.scratch;
        sl[..n].fill(0.0);
        sr[..n].fill(0.0);
        self.pool.render(&self.s, perf, &mut sl[..n], &mut sr[..n]);
        for i in 0..n {
            let (a, b) = self.chorus.process(sl[i], sr[i]);
            l[i] += a;
            r[i] += b;
        }
    }

    fn active(&self) -> usize {
        self.pool.active()
    }

    fn tail_secs(&self) -> f32 {
        self.s.aenv[3] / 1000.0 + 0.05
    }

    fn held_keys(&self, out: &mut [u8]) -> usize {
        let mut n = 0;
        for k in self.pool.gated_keys() {
            if n < out.len() {
                out[n] = k;
                n += 1;
            }
        }
        n
    }
}
