//! `fx.console` — analogue-console summing character and bus glue.
//!
//! - **Channel** goes on every track feeding a bus: it encodes the signal
//!   with `sin(x)` (clamped at ±π/2).
//! - **Buss** goes on that bus: it decodes the sum with `asin(x)`. One
//!   channel through Channel → Buss comes back unchanged (`asin(sin x) =
//!   x`), but a *sum* of encoded channels does not: the decode of
//!   `Σ sin(xᵢ)` leaves level-dependent cross terms between the channels —
//!   the intermodulation of a real summing amplifier — and nothing at all
//!   on a lone track. This is the Airwindows Console idea (Chris Johnson,
//!   MIT; PurestConsole's sin / asin pair); the implementation here is our
//!   own, run at 2× by default so the curves do not alias.
//! - **Glue** is the single-insert version for a bus without channel
//!   encoders: a slow RMS soft-knee compressor (1:1 → 2.5:1, stereo
//!   linked) into a biased-tanh "console amp" stage.
//!
//! **Iron** (all modes) saturates only the lows, as an output transformer's
//! core does: below ~120 Hz the signal goes through a tanh whose drive
//! rises with the control.
//!
//! Drive scales into the curve and back out (`f(g x) / g`), so Channel and
//! Buss at the same Drive stay exact inverses.
//!
//! Meter layout: `[input peak, output peak, gain reduction dB]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::OnePole;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.console";

pub mod p {
    pub const MODE: u32 = 0;
    pub const DRIVE: u32 = 1;
    pub const GLUE: u32 = 2;
    pub const IRON: u32 = 3;
    pub const OUTPUT: u32 = 4;
    pub const MIX: u32 = 5;
    pub const OVERSAMPLING: u32 = 6;
}

pub const MODES: &[&str] = &["Channel", "Buss", "Glue"];
const OS: &[&str] = &["1x", "2x", "4x"];

pub mod mode {
    pub const CHANNEL: usize = 0;
    pub const BUSS: usize = 1;
    pub const GLUE: usize = 2;
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, 2.0).choices(MODES),
        ParamInfo::new(p::DRIVE, "Drive", -12.0, 18.0, 0.0).unit("dB"),
        ParamInfo::new(p::GLUE, "Glue", 0.0, 100.0, 30.0).unit("%"),
        ParamInfo::new(p::IRON, "Iron", 0.0, 100.0, 20.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OVERSAMPLING, "Oversampling", 0.0, 2.0, 1.0).choices(OS).short("OS"),
    ]
}

/// Bias of the glue stage's tanh (a little 2nd harmonic).
const GLUE_BIAS: f32 = 0.15;
/// Transformer low band corner.
const IRON_HZ: f32 = 120.0;

/// The memoryless curve of a mode at linear drive `g` (unity slope).
#[inline]
pub fn curve(m: usize, x: f32, g: f32) -> f32 {
    let u = x * g;
    let y = match m {
        mode::CHANNEL => u.clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2).sin(),
        mode::BUSS => u.clamp(-1.0, 1.0).asin(),
        _ => {
            let t = GLUE_BIAS.tanh();
            ((u + GLUE_BIAS).tanh() - t) / (1.0 - t * t)
        }
    };
    y / g
}

/// Static gain reduction (dB, ≥ 0) of the glue compressor at an RMS level.
pub fn glue_gr(level_db: f32, glue: f32) -> f32 {
    let g = glue.clamp(0.0, 1.0);
    let threshold = -10.0 - 14.0 * g;
    let ratio = 1.0 + 1.5 * g;
    let knee = 12.0;
    let over = level_db - threshold;
    let slope = 1.0 - 1.0 / ratio;
    if over <= -knee / 2.0 {
        0.0
    } else if over < knee / 2.0 {
        slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
    } else {
        slope * over
    }
}

#[derive(Clone, Copy)]
struct Channel {
    os: Oversampler,
    iron: OnePole,
}

pub struct Console {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    ch: [Channel; 2],
    dry_delay: [DelayLine; 2],
    mode: usize,
    glue: f32,
    iron_drive: f32,
    iron_mix: f32,
    drive: Smoother,
    wet: Smoother,
    dry: Smoother,
    /// Glue detector: mean square and smoothed gain (dB).
    ms: f32,
    gr_db: f32,
    ms_coeff: f32,
    attack: f32,
    release: f32,
    factor: usize,
    latency: usize,
    peaks: [f32; 3],
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Console::new())
}

impl Default for Console {
    fn default() -> Self {
        Self::new()
    }
}

impl Console {
    pub fn new() -> Self {
        let params = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Console",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["saturation", "console", "summing", "bus", "glue", "mixing"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            ch: [Channel { os: Oversampler::new(2), iron: OnePole::default() }; 2],
            dry_delay: [DelayLine::new(16), DelayLine::new(16)],
            mode: mode::GLUE,
            glue: 0.3,
            iron_drive: 1.0,
            iron_mix: 0.0,
            drive: Smoother::new(1.0),
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            ms: 0.0,
            gr_db: 0.0,
            ms_coeff: 0.0,
            attack: 0.0,
            release: 0.0,
            factor: 0,
            latency: 0,
            peaks: [0.0; 3],
            first: true,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let factor = [1usize, 2, 4][self.params.choice(p::OVERSAMPLING).min(2)];
        if factor != self.factor {
            self.factor = factor;
            for c in &mut self.ch {
                c.os.set_factor(factor);
            }
            self.latency = self.ch[0].os.latency().round() as usize;
        }
        let osr = self.sr * factor as f64;
        for c in &mut self.ch {
            c.iron.set_freq(IRON_HZ, osr);
        }
        self.mode = self.params.choice(p::MODE).min(2);
        self.glue = self.params.plain(p::GLUE) / 100.0;
        let iron = self.params.plain(p::IRON) / 100.0;
        self.iron_drive = 1.0 + 5.0 * iron;
        self.iron_mix = iron.sqrt();
        self.drive.set_target(db_to_gain(self.params.plain(p::DRIVE)));
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        if self.first {
            self.drive.snap_to_target();
            self.wet.snap_to_target();
            self.dry.snap_to_target();
            self.first = false;
        }
    }
}

impl Segmented for Console {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let (factor, lat, m, glue) = (self.factor, self.latency, self.mode, self.glue);
        let (iron_drive, iron_mix) = (self.iron_drive, self.iron_mix);
        let (ms_coeff, attack, release) = (self.ms_coeff, self.attack, self.release);
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let Self { ch, dry_delay, drive: drv, wet, dry, ms, gr_db, peaks, .. } = self;
        let mut pk = *peaks;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let (l, r) = (sanitize(l), sanitize(r));
            let (g, w, d) = (drv.next(), wet.next(), dry.next());
            pk[0] = (pk[0] * decay).max(l.abs().max(r.abs()));
            // Glue: linked RMS detector and a smoothed soft-knee gain.
            let comp = if m == mode::GLUE {
                *ms += ((l * l).max(r * r) - *ms) * ms_coeff;
                let target = glue_gr(10.0 * ms.max(1e-12).log10(), glue);
                let k = if target > *gr_db { attack } else { release };
                *gr_db += (target - *gr_db) * k;
                db_to_gain(-*gr_db)
            } else {
                1.0
            };
            let mut out = [l, r];
            for (ci, x) in out.iter_mut().enumerate() {
                let c = &mut ch[ci];
                let mut up = [0.0f32; 4];
                c.os.upsample(*x * comp, &mut up);
                for s in up.iter_mut().take(factor) {
                    let y = curve(m, *s, g);
                    // Iron: saturate the band below ~120 Hz.
                    let (lo, hi) = c.iron.tick(y);
                    let sat = (lo * iron_drive).tanh() / iron_drive;
                    *s = hi + lo + iron_mix * (sat - lo);
                }
                let y = c.os.downsample(&up);
                dry_delay[ci].push(*x);
                *x = sanitize(dry_delay[ci].read(lat) * d + y * w);
            }
            pk[1] = (pk[1] * decay).max(out[0].abs().max(out[1].abs()));
            (out[0], out[1])
        });
        pk[2] = *gr_db;
        *peaks = pk.map(|v| if v.is_finite() { v } else { 0.0 });
        if !ms.is_finite() || !gr_db.is_finite() {
            *ms = 0.0;
            *gr_db = 0.0;
        }
    }
}

impl Processor for Console {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.drive, &mut self.wet, &mut self.dry] {
            s.set_time(20.0, sample_rate);
        }
        let coeff = |ms: f64| (1.0 - (-1.0 / (ms * 0.001 * sample_rate)).exp()) as f32;
        self.ms_coeff = coeff(30.0);
        self.attack = coeff(10.0);
        self.release = coeff(180.0);
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
            c.iron.reset();
        }
        for d in &mut self.dry_delay {
            d.clear();
        }
        self.ms = 0.0;
        self.gr_db = 0.0;
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

/// Peak of a meter value in dB (UI helper).
pub fn meter_db(v: f32) -> f32 {
    gain_to_db(v)
}

#[cfg(test)]
mod tests {
    use super::super::shapes::{assert_cpu, power};
    use super::*;
    use crate::test_util::{SR, process_stereo, rms, set_plain, sine};

    fn console(m: usize, os: usize) -> Console {
        let mut c = Console::new();
        set_plain(&mut c, p::MODE, m as f32);
        set_plain(&mut c, p::OVERSAMPLING, os as f32);
        set_plain(&mut c, p::IRON, 0.0);
        c
    }

    /// One channel through Channel → Buss comes back exactly (at 1×).
    #[test]
    fn channel_then_buss_is_identity() {
        for drive in [-6.0, 0.0, 6.0] {
            let mut a = console(mode::CHANNEL, 0);
            let mut b = console(mode::BUSS, 0);
            set_plain(&mut a, p::DRIVE, drive);
            set_plain(&mut b, p::DRIVE, drive);
            let input = |i: usize| {
                let v = 0.6 * (i as f32 * 0.031).sin() + 0.15 * (i as f32 * 0.17).cos();
                (v, -v)
            };
            let (l, r) = process_stereo(&mut a, input, 4800);
            let (l2, _) = process_stereo(&mut b, |i| (l[i], r[i]), 4800);
            for (i, y) in l2.iter().enumerate().skip(100) {
                let x = input(i).0;
                assert!((y - x).abs() < 1e-4, "drive {drive} at {i}: {y} vs {x}");
            }
        }
    }

    /// Summing two encoded channels and decoding the sum is not the plain
    /// sum: the (odd-order) intermodulation products 2f1 − f2 and 2f2 − f1
    /// appear.
    #[test]
    fn summed_channels_intermodulate() {
        let n = 48_000;
        let enc = |f: f32| {
            let mut c = console(mode::CHANNEL, 1);
            process_stereo(&mut c, sine(f, 0.45), n).0
        };
        let (a, b) = (enc(1000.0), enc(1300.0));
        let mut buss = console(mode::BUSS, 1);
        let (y, _) = process_stereo(&mut buss, |i| (a[i] + b[i], a[i] + b[i]), n);
        let x = &y[24_000..];
        let im = power(x, 700.0, SR) + power(x, 1600.0, SR);
        let main = power(x, 1000.0, SR);
        let im_db = 10.0 * (im / main).log10();
        assert!(im_db > -70.0 && im_db < -20.0, "IM {im_db} dB");
    }

    #[test]
    fn glue_compresses_loud_and_leaves_quiet() {
        let mut c = console(mode::GLUE, 1);
        set_plain(&mut c, p::GLUE, 100.0);
        let (q, _) = process_stereo(&mut c, sine(200.0, 0.02), 48_000);
        let gq = 20.0 * (rms(&q[24_000..]) / (0.02 / std::f32::consts::SQRT_2)).log10();
        assert!(gq.abs() < 0.5, "quiet gain {gq} dB");
        let (l, _) = process_stereo(&mut c, sine(200.0, 0.9), 48_000);
        let gl = 20.0 * (rms(&l[24_000..]) / (0.9 / std::f32::consts::SQRT_2)).log10();
        assert!(gl < -3.0, "loud gain {gl} dB");
        let mut m = [0.0f32; 3];
        c.meter(&mut m);
        assert!(m[2] > 3.0, "GR meter {}", m[2]);
    }

    #[test]
    fn iron_saturates_only_the_lows() {
        let thd = |f: f32, iron: f32| {
            let mut c = console(mode::CHANNEL, 1);
            set_plain(&mut c, p::IRON, iron);
            let (l, _) = process_stereo(&mut c, sine(f, 0.5), 48_000);
            let x = &l[24_000..];
            10.0 * (power(x, 3.0 * f as f64, SR) / power(x, f as f64, SR)).log10()
        };
        assert!(thd(50.0, 100.0) > thd(50.0, 0.0) + 10.0, "{} vs {}", thd(50.0, 100.0), thd(50.0, 0.0));
        assert!((thd(2000.0, 100.0) - thd(2000.0, 0.0)).abs() < 3.0);
    }

    #[test]
    fn flat_response_when_clean() {
        // Quiet signals pass every mode at unity, highs included.
        for m in 0..3 {
            for f in [100.0, 1000.0, 15_000.0] {
                let mut c = console(m, 1);
                set_plain(&mut c, p::GLUE, 0.0);
                let (l, _) = process_stereo(&mut c, sine(f, 0.01), 24_000);
                let g = 20.0 * (rms(&l[12_000..]) / (0.01 / std::f32::consts::SQRT_2)).log10();
                assert!(g.abs() < 0.3, "{} at {f} Hz: {g} dB", MODES[m]);
            }
        }
    }

    #[test]
    fn garbage_in_is_survived() {
        let mut c = console(mode::GLUE, 1);
        let (l, _) = process_stereo(&mut c, |i| if i % 2 == 0 { (f32::NAN, 1e9) } else { (f32::INFINITY, -1e9) }, 4800);
        assert!(l.iter().all(|v| v.is_finite()));
        let (l, _) = process_stereo(&mut c, sine(220.0, 0.3), 4800);
        assert!(rms(&l[2400..]) > 0.05);
    }

    #[test]
    fn cpu_budget() {
        let mut c = console(mode::GLUE, 2);
        assert_cpu(&mut c, 2.0);
    }
}
