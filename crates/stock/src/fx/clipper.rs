//! `fx.clipper` — mastering clipper with a ceiling, input drive, soft/hard
//! knees, a low-end *Lift*, a Delta listen switch and 1×/2×/4× oversampling.
//!
//! The curve is memoryless: the input is lifted (a fixed +6 dB low shelf at
//! 120 Hz blended by *Lift*), driven and normalised by the ceiling, shaped
//! with `tanh` (Soft) or a hard clamp (Hard), then scaled back and clamped to
//! the ceiling. With oversampling the shaper runs on the interpolated signal
//! through a first-order antiderivative (ADAA1), which is bypassed at 1× so
//! that mode stays zero-latency. At 2×/4× the reported latency is the
//! oversampler's delay plus the half high-rate sample of the ADAA.
//!
//! *Delta* outputs `clipped − input` with the dry path delayed by the same
//! latency, so only the artefacts of the clipping remain.
//!
//! Meter layout: `[0]` clipping gain reduction (dB, ≤ 0), `[1]` driven input
//! peak (dBFS), `[2]` output peak (dBFS).

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::shaper::{Adaa1, ShaperKind};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.clipper";

pub mod p {
    pub const CEILING: u32 = 0;
    pub const DRIVE: u32 = 1;
    pub const CURVE: u32 = 2;
    pub const LIFT: u32 = 3;
    pub const DELTA: u32 = 4;
    pub const OVERSAMPLING: u32 = 5;
}

/// Curve indices.
pub mod curve {
    pub const SOFT: f32 = 0.0;
    pub const HARD: f32 = 1.0;
}

const CURVES: &[&str] = &["Soft", "Hard"];
const OFF_ON: &[&str] = &["Off", "On"];
const OS: &[&str] = &["1x", "2x", "4x"];
/// Low-shelf used by *Lift* (fixed; Lift blends it in).
const LIFT_HZ: f32 = 120.0;
const LIFT_DB: f32 = 6.0;
/// Meter fall time (seconds).
const HOLD: f32 = 0.35;

/// The clipping curve in one call: `x` is driven by `drive` (linear) and
/// normalised to `ceiling` (linear, > 0).
#[inline]
pub fn curve_sample(x: f32, drive: f32, ceiling: f32, soft: bool) -> f32 {
    let ceil = ceiling.max(1e-9);
    let u = x * drive / ceil;
    let y = if soft { u.tanh() } else { u.clamp(-1.0, 1.0) };
    y * ceil
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::CEILING, "Ceiling", -12.0, 0.0, -0.3).unit("dB").short("Ceil"),
        ParamInfo::new(p::DRIVE, "Input Drive", 0.0, 24.0, 0.0).unit("dB").short("Drive"),
        ParamInfo::new(p::CURVE, "Curve", 0.0, 1.0, curve::SOFT).choices(CURVES),
        ParamInfo::new(p::LIFT, "Lift", 0.0, 100.0, 0.0).unit("%"),
        ParamInfo::new(p::DELTA, "Delta", 0.0, 1.0, 0.0).choices(OFF_ON).short("Delta"),
        ParamInfo::new(p::OVERSAMPLING, "Oversampling", 0.0, 2.0, 1.0).choices(OS).short("OS"),
    ]
}

pub struct Clipper {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    ceiling: Smoother,
    drive: Smoother,
    lift: Smoother,
    curve_soft: bool,
    delta: bool,
    lift_shelf: SvfCoeffs,
    lift_st: [SvfState; 2],
    os: [Oversampler; 2],
    shapers: [Adaa1; 2],
    dry: [DelayLine; 2],
    factor: usize,
    os_lat: [f32; 3],
    latency: usize,
    gr_hold: f32,
    in_peak: f32,
    out_peak: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Clipper::new())
}

impl Default for Clipper {
    fn default() -> Self {
        Self::new()
    }
}

impl Clipper {
    pub fn new() -> Self {
        let params = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Clipper",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "clipper", "distortion", "master"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            ceiling: Smoother::new(db_to_gain(-0.3)),
            drive: Smoother::new(1.0),
            lift: Smoother::new(0.0),
            curve_soft: true,
            delta: false,
            lift_shelf: SvfCoeffs::default(),
            lift_st: [SvfState::default(); 2],
            os: [Oversampler::new(1), Oversampler::new(1)],
            shapers: [Adaa1::new(ShaperKind::Tanh); 2],
            dry: [DelayLine::new(32), DelayLine::new(32)],
            factor: 1,
            os_lat: [0.0; 3],
            latency: 0,
            gr_hold: 1.0,
            in_peak: 0.0,
            out_peak: 0.0,
            first: true,
        };
        c.prepare(48_000.0, 512);
        c
    }

    fn update(&mut self) {
        let ps = &self.params;
        self.ceiling.set_target(db_to_gain(ps.plain(p::CEILING)));
        self.drive.set_target(db_to_gain(ps.plain(p::DRIVE)));
        self.curve_soft = ps.choice(p::CURVE) == 0;
        self.lift.set_target(ps.plain(p::LIFT) / 100.0);
        self.delta = ps.on(p::DELTA);
        for s in &mut self.shapers {
            s.set_kind(if self.curve_soft { ShaperKind::Tanh } else { ShaperKind::HardClip });
        }
        let choice = ps.choice(p::OVERSAMPLING).min(2);
        let factor = [1usize, 2, 4][choice];
        if factor != self.factor {
            self.factor = factor;
            for o in &mut self.os {
                o.set_factor(factor);
            }
            for s in &mut self.shapers {
                s.reset();
            }
        }
        // ADAA adds half a high-rate sample; it only runs above 1×.
        self.latency = if factor == 1 { 0 } else { (self.os_lat[choice] + 0.5 / factor as f32).round() as usize };
        if self.first {
            self.ceiling.snap_to_target();
            self.drive.snap_to_target();
            self.lift.snap_to_target();
            self.first = false;
        }
    }
}

impl Segmented for Clipper {
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
        let soft = self.curve_soft;
        let delta = self.delta;
        let lat = self.latency;
        let decay = (-1.0 / (HOLD * self.sr as f32)).exp();
        let this = &mut *self;
        let mut gr = this.gr_hold;
        let mut in_peak = this.in_peak;
        let mut out_peak = this.out_peak;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let drive = this.drive.next();
            let ceil = this.ceiling.next().max(1e-6);
            let lift = this.lift.next();
            let mut wet = [0.0f32; 2];
            let mut driven_peak = 0.0f32;
            let mut gr_min = 1.0f32;
            for c in 0..2 {
                let x = if c == 0 { l } else { r };
                // Lift: fixed low shelf blended in by the Lift amount.
                let shelf = this.lift_st[c].process(&this.lift_shelf, x);
                let x = x + (shelf - x) * lift;
                let mut up = [0.0f32; 4];
                this.os[c].upsample(x, &mut up);
                for v in up.iter_mut().take(factor) {
                    let pre = *v * drive;
                    driven_peak = driven_peak.max(pre.abs());
                    let u = pre / ceil;
                    let y = if factor > 1 {
                        this.shapers[c].process(u)
                    } else if soft {
                        u.tanh()
                    } else {
                        u.clamp(-1.0, 1.0)
                    };
                    let post = y * ceil;
                    let g = if pre.abs() > 1e-12 { (post / pre).abs().min(1.0) } else { 1.0 };
                    gr_min = gr_min.min(g);
                    *v = post;
                }
                wet[c] = this.os[c].downsample(&up);
            }
            // Dry path, aligned to the oversampler + ADAA delay.
            this.dry[0].push(l);
            this.dry[1].push(r);
            let dry = [this.dry[0].read(lat), this.dry[1].read(lat)];
            let out = if delta {
                [wet[0] - dry[0], wet[1] - dry[1]]
            } else {
                [wet[0].clamp(-ceil, ceil), wet[1].clamp(-ceil, ceil)]
            };
            gr = (gr / decay).min(gr_min);
            in_peak = (in_peak * decay).max(driven_peak);
            out_peak = (out_peak * decay).max(out[0].abs().max(out[1].abs()));
            (sanitize(out[0]), sanitize(out[1]))
        });
        self.gr_hold = gr;
        self.in_peak = in_peak;
        self.out_peak = out_peak;
    }
}

impl Processor for Clipper {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.ceiling.set_time(5.0, sample_rate);
        self.drive.set_time(20.0, sample_rate);
        self.lift.set_time(20.0, sample_rate);
        self.lift_shelf = SvfCoeffs::new(SvfMode::LowShelf, LIFT_HZ, std::f32::consts::FRAC_1_SQRT_2, LIFT_DB, sample_rate);
        self.os_lat = [Oversampler::new(1).latency(), Oversampler::new(2).latency(), Oversampler::new(4).latency()];
        self.os = [Oversampler::new(1), Oversampler::new(1)];
        self.dry = [DelayLine::new(64), DelayLine::new(64)];
        self.factor = 0;
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for o in &mut self.os {
            o.reset();
        }
        for s in &mut self.shapers {
            s.reset();
        }
        for d in &mut self.dry {
            d.clear();
        }
        for s in &mut self.lift_st {
            s.reset();
        }
        self.gr_hold = 1.0;
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

    fn latency(&self) -> u32 {
        let choice = self.params.choice(p::OVERSAMPLING).min(2);
        if choice == 0 { 0 } else { (self.os_lat[choice] + 0.5 / [1usize, 2, 4][choice] as f32).round() as u32 }
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[gain_to_db(self.gr_hold).min(0.0), gain_to_db(self.in_peak), gain_to_db(self.out_peak)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, rms, set_plain, sine};

    fn level(x: &[f32]) -> f32 {
        gain_to_db(rms(x))
    }

    #[test]
    fn curve_is_bounded_and_monotonic() {
        for soft in [false, true] {
            let ceil = db_to_gain(-1.0);
            let mut last = f32::MIN;
            for i in 0..=200 {
                let x = -2.0 + 4.0 * i as f32 / 200.0;
                let y = curve_sample(x, db_to_gain(12.0), ceil, soft);
                assert!(y.abs() <= ceil + 1e-6, "{soft}: {y}");
                assert!(y >= last - 1e-5, "{soft}: not monotonic at {x}");
                last = y;
            }
        }
    }

    #[test]
    fn never_exceeds_the_ceiling() {
        for os in [0.0f32, 1.0, 2.0] {
            for curve in [curve::SOFT, curve::HARD] {
                let mut c = Clipper::new();
                set_plain(&mut c, p::CEILING, -1.0);
                set_plain(&mut c, p::DRIVE, 12.0);
                set_plain(&mut c, p::CURVE, curve);
                set_plain(&mut c, p::OVERSAMPLING, os);
                let (l, _) = process_stereo(&mut c, |i| {
                    let t = i as f64 / SR;
                    let v = (1.5 * (std::f64::consts::TAU * 997.0 * t).sin()) as f32;
                    (v, v)
                }, 24_000);
                let skip = c.latency() as usize + 1000;
                assert!(peak(&l[skip..]) <= db_to_gain(-1.0) + 1e-4, "os {os} curve {curve}: {}", peak(&l[skip..]));
            }
        }
    }

    #[test]
    fn delta_is_silent_when_nothing_clips() {
        let mut c = Clipper::new();
        set_plain(&mut c, p::CEILING, -1.0);
        set_plain(&mut c, p::DRIVE, 0.0);
        set_plain(&mut c, p::CURVE, curve::HARD);
        set_plain(&mut c, p::DELTA, 1.0);
        set_plain(&mut c, p::OVERSAMPLING, 0.0);
        let (l, _) = process_stereo(&mut c, sine(440.0, 0.3), 4800);
        assert!(peak(&l[480..]) < 1e-6, "{}", peak(&l));
    }

    #[test]
    fn delta_reveals_the_clipping_artefacts() {
        let mut c = Clipper::new();
        set_plain(&mut c, p::CEILING, -6.0);
        set_plain(&mut c, p::DRIVE, 12.0);
        set_plain(&mut c, p::DELTA, 1.0);
        set_plain(&mut c, p::OVERSAMPLING, 0.0);
        let (l, _) = process_stereo(&mut c, sine(440.0, 0.5), 4800);
        assert!(peak(&l[480..]) > 0.01);
    }

    #[test]
    fn lift_raises_the_low_end() {
        let gain_at = |freq: f32, lift: f32| {
            let mut c = Clipper::new();
            set_plain(&mut c, p::CEILING, 0.0);
            set_plain(&mut c, p::DRIVE, 0.0);
            set_plain(&mut c, p::CURVE, curve::HARD);
            set_plain(&mut c, p::LIFT, lift);
            set_plain(&mut c, p::OVERSAMPLING, 0.0);
            let (l, _) = process_stereo(&mut c, sine(freq, 0.1), 24_000);
            level(&l[12_000..]) - level(&[0.1 / std::f32::consts::SQRT_2; 1])
        };
        assert!(gain_at(80.0, 100.0) > 3.0);
        assert!(gain_at(4_000.0, 100.0).abs() < 0.5);
    }

    #[test]
    fn drive_increases_the_reported_reduction() {
        let mut quiet = Clipper::new();
        set_plain(&mut quiet, p::CEILING, -6.0);
        set_plain(&mut quiet, p::DRIVE, 0.0);
        set_plain(&mut quiet, p::CURVE, curve::HARD);
        set_plain(&mut quiet, p::OVERSAMPLING, 0.0);
        process_stereo(&mut quiet, sine(440.0, 0.2), 4800);
        let mut loud = Clipper::new();
        set_plain(&mut loud, p::CEILING, -6.0);
        set_plain(&mut loud, p::DRIVE, 18.0);
        set_plain(&mut loud, p::CURVE, curve::HARD);
        set_plain(&mut loud, p::OVERSAMPLING, 0.0);
        process_stereo(&mut loud, sine(440.0, 0.2), 4800);
        let mut a = [0.0f32; 3];
        let mut b = [0.0f32; 3];
        assert_eq!(quiet.meter(&mut a), 3);
        assert_eq!(loud.meter(&mut b), 3);
        assert!(a[0] > -0.5, "{}", a[0]);
        assert!(b[0] < -3.0, "{}", b[0]);
        assert!(b[2] <= -6.0 + 0.01, "{}", b[2]);
    }

    #[test]
    fn latency_switches_with_oversampling() {
        let mut c = Clipper::new();
        set_plain(&mut c, p::OVERSAMPLING, 0.0);
        process_stereo(&mut c, sine(440.0, 0.1), 128);
        assert_eq!(c.latency(), 0);
        set_plain(&mut c, p::OVERSAMPLING, 1.0);
        assert!(c.latency() > 0);
        set_plain(&mut c, p::OVERSAMPLING, 2.0);
        assert!(c.latency() > 0);
        process_stereo(&mut c, sine(440.0, 0.1), 512);
        assert_eq!(c.latency(), (c.os_lat[2] + 0.5 / 4.0).round() as u32);
    }
}
