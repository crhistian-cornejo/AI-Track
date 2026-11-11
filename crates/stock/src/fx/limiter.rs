//! `fx.limiter` — lookahead brickwall limiter with true-peak detection.
//!
//! Per sample the gain the peak needs (`ceiling / peak`) goes through a
//! sliding minimum as long as the lookahead, an instant-attack /
//! smooth-release stage and a moving average of the same length, so the gain
//! reaches its target exactly at the peak without overshoot and never rises
//! above what any sample in its window needs. With true peak on, detection
//! uses a 4× polyphase interpolator (BS.1770 method) plus the sample values,
//! held over the interpolator's span so the gain is flat wherever the
//! interpolated waveform is evaluated.
//!
//! Oversampling (2×/4×, polyphase IIR) runs the whole limiter at the high
//! rate; a short base-rate true-peak stage then catches what the decimation
//! filter's ringing adds. A final clip at the ceiling guarantees the sample
//! peak. The internal target sits 0.02 dB under the ceiling to absorb
//! rounding. Release can be fixed or adaptive (fast for transient
//! reduction, slow for sustained reduction).
//!
//! `latency()` = lookahead + detection span + oversampler delay (rounded).
//!
//! Meter layout: `[0]` gain reduction dB (≤ 0), `[1]` output true peak
//! dBTP, `[2]` input peak dBFS (after input gain).

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::dynamics::{BoxFilter, SlidingMax, TruePeak};
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::smooth::{Smoother, one_pole_coeff};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.limiter";

pub mod p {
    pub const GAIN: u32 = 0;
    pub const CEILING: u32 = 1;
    pub const RELEASE: u32 = 2;
    pub const AUTO_RELEASE: u32 = 3;
    pub const LOOKAHEAD: u32 = 4;
    pub const TRUE_PEAK: u32 = 5;
    pub const OVERSAMPLING: u32 = 6;
    pub const LINK: u32 = 7;
}

const OFF_ON: &[&str] = &["Off", "On"];
const OS: &[&str] = &["1x", "2x", "4x"];
const MAX_LOOKAHEAD_MS: f32 = 10.0;
/// Internal target below the ceiling.
const MARGIN_DB: f32 = 0.02;
const CONTROL: usize = 16;
/// Span of the true-peak hold (covers the interpolator's 16 taps).
const HOLD: usize = 17;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::GAIN, "Input Gain", 0.0, 24.0, 0.0).unit("dB").short("Gain"),
        ParamInfo::new(p::CEILING, "Ceiling", -12.0, 0.0, -1.0).unit("dB").short("Ceil"),
        ParamInfo::new(p::RELEASE, "Release", 1.0, 1_000.0, 80.0).unit("ms").log().short("Rel"),
        ParamInfo::new(p::AUTO_RELEASE, "Auto Release", 0.0, 1.0, 1.0).choices(OFF_ON).short("Auto"),
        ParamInfo::new(p::LOOKAHEAD, "Lookahead", 0.5, MAX_LOOKAHEAD_MS, 1.5).unit("ms").short("Look"),
        ParamInfo::new(p::TRUE_PEAK, "True Peak", 0.0, 1.0, 1.0).choices(OFF_ON).short("TP"),
        ParamInfo::new(p::OVERSAMPLING, "Oversampling", 0.0, 2.0, 0.0).choices(OS).short("OS"),
        ParamInfo::new(p::LINK, "Stereo Link", 0.0, 100.0, 100.0).unit("%").short("Link"),
    ]
}

/// One limiter running at one rate.
struct Core {
    tp_on: bool,
    tp: [TruePeak; 2],
    tp_prev: [f32; 2],
    xdel: [DelayLine; 2],
    hold: [SlidingMax; 2],
    smin: [SlidingMax; 2],
    avg: [BoxFilter; 2],
    rel: [f32; 2],
    rel_coeff: f32,
    main: [DelayLine; 2],
    main_delay: usize,
    target: f32,
    link: f32,
    last_gain: f32,
    max_window: usize,
}

impl Core {
    fn new(max_window: usize) -> Self {
        let det = TruePeak::LATENCY + HOLD;
        Self {
            tp_on: true,
            tp: [TruePeak::new(), TruePeak::new()],
            tp_prev: [0.0; 2],
            xdel: [DelayLine::new(TruePeak::LATENCY + 1), DelayLine::new(TruePeak::LATENCY + 1)],
            hold: [SlidingMax::new(HOLD), SlidingMax::new(HOLD)],
            smin: [SlidingMax::new(max_window), SlidingMax::new(max_window)],
            avg: [BoxFilter::new(max_window), BoxFilter::new(max_window)],
            rel: [1.0; 2],
            rel_coeff: 0.001,
            main: [DelayLine::new(max_window + det + 2), DelayLine::new(max_window + det + 2)],
            main_delay: 0,
            target: 1.0,
            link: 1.0,
            last_gain: 1.0,
            max_window: max_window.max(1),
        }
    }

    /// Detection delay in samples.
    fn det_delay(&self) -> usize {
        if self.tp_on { TruePeak::LATENCY + HOLD / 2 } else { 0 }
    }

    fn configure(&mut self, window: usize, tp_on: bool) {
        let w = window.clamp(1, self.max_window);
        self.tp_on = tp_on;
        for c in 0..2 {
            self.smin[c].set_window(w);
            self.avg[c].set_window(w, 1.0);
            self.hold[c].set_window(HOLD);
        }
        self.main_delay = self.det_delay() + w - 1;
        self.reset();
    }

    fn latency(&self) -> usize {
        self.main_delay
    }

    fn reset(&mut self) {
        for c in 0..2 {
            self.tp[c].reset();
            self.xdel[c].clear();
            self.hold[c].reset();
            self.smin[c].reset();
            let w = self.smin[c].window();
            self.avg[c].set_window(w, 1.0);
            self.main[c].clear();
        }
        self.tp_prev = [0.0; 2];
        self.rel = [1.0; 2];
        self.last_gain = 1.0;
    }

    #[inline]
    fn detect(&mut self, c: usize, x: f32) -> f32 {
        if self.tp_on {
            let t = self.tp[c].process(x);
            self.xdel[c].push(x);
            let xs = self.xdel[c].read(TruePeak::LATENCY).abs();
            let v = t.max(self.tp_prev[c]).max(xs);
            self.tp_prev[c] = t;
            self.hold[c].process(v)
        } else {
            x.abs()
        }
    }

    #[inline]
    fn process(&mut self, l: f32, r: f32) -> (f32, f32) {
        let x = [l, r];
        let mut req = [1.0f32; 2];
        for c in 0..2 {
            let d = self.detect(c, x[c]);
            if d > self.target {
                req[c] = self.target / d;
            }
        }
        let linked = req[0].min(req[1]);
        let mut out = [0.0f32; 2];
        let mut gmin = 1.0f32;
        for c in 0..2 {
            let g = req[c] + (linked - req[c]) * self.link;
            let m = 1.0 - self.smin[c].process(1.0 - g);
            let rel = if m < self.rel[c] { m } else { self.rel[c] + (m - self.rel[c]) * self.rel_coeff };
            self.rel[c] = rel;
            let gs = self.avg[c].process(rel).min(1.0);
            self.main[c].push(x[c]);
            out[c] = self.main[c].read(self.main_delay) * gs;
            gmin = gmin.min(gs);
        }
        self.last_gain = gmin;
        (out[0], out[1])
    }
}

pub struct Limiter {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    core: Core,
    safety: Core,
    os: [Oversampler; 2],
    factor: usize,
    ceiling: f32,
    gain: Smoother,
    release_ms: f32,
    auto_release: bool,
    gr_slow: f32,
    slow_coeff: f32,
    out_tp: [TruePeak; 2],
    meter_gr: f32,
    meter_tp: f32,
    meter_in: f32,
    config: (usize, usize, bool),
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Limiter::new())
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Limiter {
    pub fn new() -> Self {
        let params = params();
        let mut l = Self {
            info: PluginInfo {
                id: ID,
                name: "Limiter",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "limiter", "true peak", "master"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            core: Core::new(16),
            safety: Core::new(64),
            os: [Oversampler::new(1), Oversampler::new(1)],
            factor: 1,
            ceiling: db_to_gain(-1.0),
            gain: Smoother::new(1.0),
            release_ms: 80.0,
            auto_release: true,
            gr_slow: 0.0,
            slow_coeff: 0.0,
            out_tp: [TruePeak::new(), TruePeak::new()],
            meter_gr: 0.0,
            meter_tp: -120.0,
            meter_in: -120.0,
            config: (0, 0, false),
            first: true,
        };
        l.prepare(48_000.0, 512);
        l
    }

    fn update(&mut self) {
        let ps = &self.params;
        self.ceiling = db_to_gain(ps.plain(p::CEILING));
        let target = self.ceiling * db_to_gain(-MARGIN_DB);
        self.core.target = target;
        self.safety.target = target;
        self.core.link = ps.plain(p::LINK) / 100.0;
        self.safety.link = self.core.link;
        self.gain.set_target(db_to_gain(ps.plain(p::GAIN)));
        if self.first {
            self.gain.snap_to_target();
            self.first = false;
        }
        self.release_ms = ps.plain(p::RELEASE);
        self.auto_release = ps.on(p::AUTO_RELEASE);
        let factor = [1usize, 2, 4][ps.choice(p::OVERSAMPLING).min(2)];
        let tp = ps.on(p::TRUE_PEAK);
        let w = ((ps.plain(p::LOOKAHEAD) * 0.001) as f64 * self.sr * factor as f64).round() as usize;
        let cfg = (factor, w, tp);
        if cfg != self.config {
            self.config = cfg;
            self.factor = factor;
            for o in &mut self.os {
                o.set_factor(factor);
            }
            self.core.configure(w.max(1), tp);
            let sw = ((0.5e-3 * self.sr).round() as usize).max(1);
            self.safety.configure(sw, true);
            self.safety.rel_coeff = one_pole_coeff(20.0, self.sr);
        }
        self.set_release(self.release_ms);
    }

    fn set_release(&mut self, ms: f32) {
        self.core.rel_coeff = one_pole_coeff(ms, self.sr * self.factor as f64);
    }

    fn total_latency(&self) -> f32 {
        if self.factor == 1 {
            self.core.latency() as f32
        } else {
            self.core.latency() as f32 / self.factor as f32 + self.os[0].latency() + self.safety.latency() as f32
        }
    }
}

impl Segmented for Limiter {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let ceil = self.ceiling;
        let factor = self.factor;
        let mut gr_min = 1.0f32;
        let mut tp_max = db_to_gain(self.meter_tp) * 0.7;
        let mut in_max = db_to_gain(self.meter_in) * 0.7;
        let mut i = start;
        while i < end {
            let e = (i + CONTROL).min(end);
            if self.auto_release {
                let gr = -gain_to_db(self.core.last_gain);
                self.gr_slow += (gr - self.gr_slow) * self.slow_coeff;
                let sustain = if gr > 0.1 { (self.gr_slow / gr).clamp(0.0, 1.0) } else { 1.0 };
                self.set_release(self.release_ms * (0.3 + 1.7 * sustain));
            }
            let this = &mut *self;
            for_frames(ctx.buffer, i, e, |_, l, r| {
                let g = this.gain.next();
                let (l, r) = (sanitize(l * g), sanitize(r * g));
                in_max = in_max.max(l.abs()).max(r.abs());
                let (mut ol, mut or) = if factor == 1 {
                    this.core.process(l, r)
                } else {
                    let mut ul = [0.0f32; 4];
                    let mut ur = [0.0f32; 4];
                    this.os[0].upsample(l, &mut ul);
                    this.os[1].upsample(r, &mut ur);
                    for k in 0..factor {
                        let (a, b) = this.core.process(ul[k], ur[k]);
                        ul[k] = a;
                        ur[k] = b;
                    }
                    let dl = this.os[0].downsample(&ul);
                    let dr = this.os[1].downsample(&ur);
                    this.safety.process(dl, dr)
                };
                gr_min = gr_min.min(this.core.last_gain);
                ol = ol.clamp(-ceil, ceil);
                or = or.clamp(-ceil, ceil);
                tp_max = tp_max.max(this.out_tp[0].process(ol)).max(this.out_tp[1].process(or));
                (ol, or)
            });
            i = e;
        }
        self.meter_gr = gain_to_db(gr_min).min(0.0);
        self.meter_tp = gain_to_db(tp_max);
        self.meter_in = gain_to_db(in_max);
    }
}

impl Processor for Limiter {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        let max_w = ((MAX_LOOKAHEAD_MS * 0.001) as f64 * sample_rate * 4.0).ceil() as usize + 2;
        self.core = Core::new(max_w);
        self.safety = Core::new(((1e-3 * sample_rate).ceil() as usize).max(4));
        self.gain.set_time(20.0, sample_rate);
        self.slow_coeff = one_pole_coeff(500.0, sample_rate / CONTROL as f64);
        self.config = (0, 0, false);
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.core.reset();
        self.safety.reset();
        for o in &mut self.os {
            o.reset();
        }
        for t in &mut self.out_tp {
            t.reset();
        }
        self.gr_slow = 0.0;
        self.meter_gr = 0.0;
        self.meter_tp = -120.0;
        self.meter_in = -120.0;
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
        // Reflect the current parameters even before the next block.
        let ps = &self.params;
        let factor = [1usize, 2, 4][ps.choice(p::OVERSAMPLING).min(2)];
        let tp = ps.on(p::TRUE_PEAK);
        let w = (((ps.plain(p::LOOKAHEAD) * 0.001) as f64 * self.sr * factor as f64).round() as usize).max(1);
        if (factor, w, tp) == self.config {
            return self.total_latency().round() as u32;
        }
        let det = if tp { TruePeak::LATENCY + HOLD / 2 } else { 0 };
        let core = (det + w - 1) as f32 / factor as f32;
        let extra = if factor == 1 {
            0.0
        } else {
            Oversampler::new(factor).latency() + (TruePeak::LATENCY + HOLD / 2 + ((0.5e-3 * self.sr).round() as usize).max(1) - 1) as f32
        };
        (core + extra).round() as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.meter_gr, self.meter_tp, self.meter_in])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{process_stereo, set_plain};
    use aitrack_dsp::osc::Noise;

    /// Worst output true peak (dBTP) and sample peak after the latency.
    fn worst(lim: &mut Limiter, input: &mut dyn FnMut(usize) -> (f32, f32), n: usize) -> (f32, f32) {
        let (l, r) = process_stereo(lim, input, n);
        let skip = lim.latency() as usize + 64;
        let mut tp = [TruePeak::new(), TruePeak::new()];
        let mut worst_tp = 0.0f32;
        let mut worst_sp = 0.0f32;
        for i in 0..n {
            let a = tp[0].process(l[i]);
            let b = tp[1].process(r[i]);
            if i > skip {
                worst_tp = worst_tp.max(a).max(b);
                worst_sp = worst_sp.max(l[i].abs()).max(r[i].abs());
            }
        }
        (gain_to_db(worst_tp), gain_to_db(worst_sp))
    }

    #[test]
    fn never_exceeds_the_ceiling() {
        let n = 48_000;
        type Signal = Box<dyn Fn() -> Box<dyn FnMut(usize) -> (f32, f32)>>;
        let signals: Vec<(&str, Signal)> = vec![
            (
                "noise +12",
                Box::new(|| {
                    let mut a = Noise::new(1);
                    let mut b = Noise::new(2);
                    Box::new(move |_| (a.next() * 4.0, b.next() * 4.0))
                }),
            ),
            (
                "fs/4 45°",
                Box::new(|| {
                    Box::new(|i| {
                        let v = 2.0 * (std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4).sin();
                        (v, -v)
                    })
                }),
            ),
            ("square", Box::new(|| Box::new(|i| if (i / 37) % 2 == 0 { (1.8, 1.8) } else { (-1.8, -1.8) }))),
            (
                "drums",
                Box::new(|| {
                    let mut nz = Noise::new(9);
                    Box::new(move |i| {
                        let t = (i % 6000) as f32 / 48_000.0;
                        let kick = 3.0 * (-t * 30.0).exp() * (std::f32::consts::TAU * 55.0 * t * (1.0 + 2.0 * (-t * 40.0).exp())).sin();
                        let click = if i % 6000 == 0 { 4.0 } else { 0.0 };
                        let hat = if (i + 3000) % 6000 < 400 { nz.next() * 2.0 } else { 0.0 };
                        let v = kick + click + hat;
                        (v, v * 0.8)
                    })
                }),
            ),
            (
                "hf sines",
                Box::new(|| {
                    Box::new(|i| {
                        let t = i as f64 / 48_000.0;
                        let v = 1.5 * (std::f64::consts::TAU * 19_000.0 * t).sin() + 0.8 * (std::f64::consts::TAU * 11_123.0 * t).sin();
                        (v as f32, (v * 0.7) as f32)
                    })
                }),
            ),
        ];
        let mut excess = f32::MIN;
        for os in [0.0f32, 1.0, 2.0] {
            for ceiling in [-1.0f32, -0.1, -6.0] {
                for (name, make) in &signals {
                    let mut lim = Limiter::new();
                    set_plain(&mut lim, p::OVERSAMPLING, os);
                    set_plain(&mut lim, p::CEILING, ceiling);
                    set_plain(&mut lim, p::GAIN, 6.0);
                    let mut sig = make();
                    let (tp, sp) = worst(&mut lim, &mut *sig, n);
                    excess = excess.max(tp - ceiling);
                    assert!(tp <= ceiling + 0.05, "{name} os {os} ceiling {ceiling}: true peak {tp} dBTP");
                    assert!(sp <= ceiling + 1e-4, "{name} os {os} ceiling {ceiling}: sample peak {sp}");
                }
            }
        }
        println!("worst true peak relative to the ceiling: {excess:+.3} dB");
    }

    #[test]
    fn quiet_signal_passes_unchanged_after_latency() {
        let mut lim = Limiter::new();
        let lat = lim.latency() as usize;
        let sig = |i: usize| {
            let v = 0.3 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin();
            (v, v)
        };
        let (l, _) = process_stereo(&mut lim, sig, 9600);
        for i in lat + 100..9600 {
            assert!((l[i] - sig(i - lat).0).abs() < 1e-4, "at {i}");
        }
    }

    #[test]
    fn latency_reports_lookahead() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::TRUE_PEAK, 0.0);
        set_plain(&mut lim, p::LOOKAHEAD, 2.0);
        let l = lim.latency();
        assert_eq!(l, 95);
        set_plain(&mut lim, p::OVERSAMPLING, 2.0);
        assert!(lim.latency() > 95);
        let mut m = [0.0f32; 3];
        assert_eq!(lim.meter(&mut m), 3);
    }

    #[test]
    fn reduces_loud_input() {
        let mut lim = Limiter::new();
        set_plain(&mut lim, p::GAIN, 12.0);
        let (l, _) = process_stereo(&mut lim, |i| {
            let v = 0.5 * (std::f32::consts::TAU * 100.0 * i as f32 / 48_000.0).sin();
            (v, v)
        }, 24_000);
        let mut m = [0.0f32; 3];
        lim.meter(&mut m);
        assert!(m[0] < -6.5 && m[0] > -7.5, "gr {}", m[0]);
        let pk = l[12_000..].iter().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!(gain_to_db(pk) > -1.5 && gain_to_db(pk) <= -1.0);
    }
}
