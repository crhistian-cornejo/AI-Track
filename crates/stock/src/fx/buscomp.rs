//! `fx.buscomp` — bus compressor of the "glue" family: a feed-forward VCA-style
//! design with a log-domain level detector, a short quadratic knee, fixed
//! ratios and stepped times, built for the stereo bus and the drum bus. It is
//! a separate, deliberately small device next to `fx.compressor` (23 free
//! parameters); this one has twelve and every time is a click-stop.
//!
//! * **Detector** — half the rectified peak of the louder channel (held for
//!   1.5 ms so a steady tone reads without ripple), half a 10 ms RMS
//!   (scaled so a sine reads as its peak), mixed linearly and taken to dB.
//!   One detector for both channels: the bus is compressed as one signal.
//! * **Gain computer** — Giannoulis, Massberg & Reiss (JAES 2012) with a fixed
//!   4 dB quadratic knee and ratios 2, 4 and 10 to 1. `Range` caps the
//!   reduction.
//! * **Ballistics** — branching: the attack time constant is a click-stop from
//!   0.01 to 30 ms (slow ones let the transient through); the release is a
//!   click-stop from 0.1 to 1.2 s or **Auto**, a program-dependent release
//!   with two-pole memory (fast for hits, slow for sustained reduction), see
//!   [`AutoReleaseSmoother`].
//! * **Sidechain** — the input or the external key, through a 2nd-order
//!   Butterworth high-pass (Off, 30 … 240 Hz) so the low end does not trigger
//!   the compression; `SC Listen` plays the filtered key.
//! * **Soft clip** — a `tanh` waveshaper with its ceiling at −0.5 dBFS after
//!   the makeup, ADAA1 on a 2× oversampler. Only when it is on does the
//!   device report latency; the dry path of `Mix` is delayed to match. The
//!   switch fades between the two paths over 5 ms (the clipped and delayed
//!   one comes in while the plain one goes out), as does `SC Listen`, so
//!   neither clicks when automated.
//!
//! Meter layout (70 values, the same shape as `fx.compressor`): `[0]` gain
//! reduction in dB (≤ 0, peak-held with a 40 dB/s fall), `[1]` input peak
//! dBFS, `[2]` output peak dBFS, `[3..67)` static curve: output level in dB
//! for inputs from −60 to 0 dB in 64 steps (makeup included), `[67]` latency
//! in samples, `[68]` 1 while the soft clip is working hard (held 250 ms),
//! `[69]` the release time in effect, in ms.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, sidechain_frame, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::envelope::{AutoReleaseSmoother, PeakHold, gain_computer};
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::shaper::{Adaa1, ShaperKind};
use aitrack_dsp::smooth::{LinearRamp, Smoother, one_pole_coeff};
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.buscomp";

pub mod p {
    pub const THRESHOLD: u32 = 0;
    pub const RATIO: u32 = 1;
    pub const ATTACK: u32 = 2;
    pub const RELEASE: u32 = 3;
    pub const RANGE: u32 = 4;
    pub const MAKEUP: u32 = 5;
    pub const SOFT_CLIP: u32 = 6;
    pub const MIX: u32 = 7;
    pub const SIDECHAIN: u32 = 8;
    pub const SC_HPF: u32 = 9;
    pub const SC_LISTEN: u32 = 10;
    pub const OUTPUT: u32 = 11;
}

/// `RATIO` choices.
pub const RATIOS: [f32; 3] = [2.0, 4.0, 10.0];
/// `ATTACK` choices, ms (time constants).
pub const ATTACKS_MS: [f32; 7] = [0.01, 0.1, 0.3, 1.0, 3.0, 10.0, 30.0];
/// `RELEASE` choices 0..6, ms (time constants); choice 6 is Auto.
pub const RELEASES_MS: [f32; 6] = [100.0, 200.0, 400.0, 600.0, 800.0, 1_200.0];
/// `RELEASE` choice index of Auto.
pub const RELEASE_AUTO: usize = 6;
/// `SC_HPF` choices, Hz; 0 = Off.
pub const SC_HPF_HZ: [f32; 7] = [0.0, 30.0, 60.0, 90.0, 120.0, 180.0, 240.0];
/// Fixed knee width.
pub const KNEE_DB: f32 = 4.0;
/// Soft clip ceiling.
pub const CEILING_DB: f32 = -0.5;

const OFF_ON: &[&str] = &["Off", "On"];
const SC: &[&str] = &["Internal", "External"];
const RATIO_NAMES: &[&str] = &["2:1", "4:1", "10:1"];
const ATTACK_NAMES: &[&str] = &["0.01 ms", "0.1 ms", "0.3 ms", "1 ms", "3 ms", "10 ms", "30 ms"];
const RELEASE_NAMES: &[&str] = &["0.1 s", "0.2 s", "0.4 s", "0.6 s", "0.8 s", "1.2 s", "Auto"];
const HPF_NAMES: &[&str] = &["Off", "30 Hz", "60 Hz", "90 Hz", "120 Hz", "180 Hz", "240 Hz"];
/// Fall of the gain-reduction meter, dB per second.
const GR_FALL_DB_S: f32 = 40.0;
/// Auto release: fast end, slow end (ms) and the reduction depth that
/// reaches the slow end.
const AUTO_FAST_MS: f32 = 100.0;
const AUTO_SLOW_MS: f32 = 1_200.0;
const AUTO_DEPTH_DB: f32 = 10.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::THRESHOLD, "Threshold", -40.0, 0.0, -12.0).unit("dB").short("Thresh"),
        ParamInfo::new(p::RATIO, "Ratio", 0.0, 2.0, 1.0).choices(RATIO_NAMES),
        ParamInfo::new(p::ATTACK, "Attack", 0.0, 6.0, 5.0).choices(ATTACK_NAMES).short("Att"),
        ParamInfo::new(p::RELEASE, "Release", 0.0, 6.0, 6.0).choices(RELEASE_NAMES).short("Rel"),
        ParamInfo::new(p::RANGE, "Range", 0.0, 40.0, 40.0).unit("dB"),
        ParamInfo::new(p::MAKEUP, "Makeup", 0.0, 24.0, 0.0).unit("dB"),
        ParamInfo::new(p::SOFT_CLIP, "Soft Clip", 0.0, 1.0, 0.0).choices(OFF_ON).short("Clip"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::SIDECHAIN, "Sidechain", 0.0, 1.0, 0.0).choices(SC).short("SC"),
        ParamInfo::new(p::SC_HPF, "SC High-Pass", 0.0, 6.0, 2.0).choices(HPF_NAMES).short("SC HPF"),
        ParamInfo::new(p::SC_LISTEN, "SC Listen", 0.0, 1.0, 0.0).choices(OFF_ON).short("Listen"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 24.0, 0.0).unit("dB").short("Out"),
    ]
}

/// The level detector, gain computer and ballistics of the bus compressor,
/// shared with the devices that embed it (`fx.drumbus`).
#[derive(Clone, Copy, Debug)]
pub struct BusGain {
    hold: PeakHold,
    ms: [f32; 2],
    rms_coeff: f32,
    smoother: AutoReleaseSmoother,
}

impl Default for BusGain {
    fn default() -> Self {
        Self::new()
    }
}

impl BusGain {
    pub fn new() -> Self {
        let mut b = Self { hold: PeakHold::new(), ms: [0.0; 2], rms_coeff: 0.01, smoother: AutoReleaseSmoother::default() };
        b.prepare(48_000.0);
        b
    }

    pub fn prepare(&mut self, sample_rate: f64) {
        self.hold.set(1.5, 5.0, sample_rate);
        self.rms_coeff = one_pole_coeff(10.0, sample_rate);
        self.reset();
    }

    pub fn set_attack_ms(&mut self, ms: f32, sample_rate: f64) {
        self.smoother.set_attack_ms(ms, sample_rate);
    }

    /// `Some(ms)`: fixed release; `None`: Auto.
    pub fn set_release(&mut self, ms: Option<f32>, sample_rate: f64) {
        match ms {
            Some(ms) => self.smoother.set_release_ms(ms, sample_rate),
            None => self.smoother.set_auto(AUTO_FAST_MS, AUTO_SLOW_MS, AUTO_DEPTH_DB, sample_rate),
        }
    }

    /// Release time constant in effect, ms.
    pub fn release_ms(&self, fixed: Option<f32>) -> f32 {
        fixed.unwrap_or_else(|| self.smoother.current_release_ms())
    }

    /// Detector level in dB of a stereo key sample.
    #[inline]
    pub fn level_db(&mut self, dl: f32, dr: f32) -> f32 {
        let peak = self.hold.process(dl.abs().max(dr.abs()));
        self.ms[0] += (dl * dl - self.ms[0]) * self.rms_coeff;
        self.ms[1] += (dr * dr - self.ms[1]) * self.rms_coeff;
        for m in &mut self.ms {
            if *m < 1e-30 {
                *m = 0.0;
            }
        }
        let rms = (2.0 * self.ms[0].max(self.ms[1])).sqrt(); // sine RMS → peak scale
        gain_to_db(0.5 * (peak + rms))
    }

    /// One sample of the key through detector, gain computer and smoother:
    /// the smoothed gain reduction in dB (≥ 0).
    #[inline]
    pub fn process(&mut self, dl: f32, dr: f32, threshold_db: f32, ratio: f32, range_db: f32) -> f32 {
        let level = self.level_db(dl, dr);
        let target = gain_computer(level, threshold_db, ratio, KNEE_DB).min(range_db);
        self.smoother.process(target)
    }

    pub fn reset(&mut self) {
        self.hold.reset();
        self.ms = [0.0; 2];
        self.smoother.reset();
    }
}

pub struct BusComp {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    thr: f32,
    ratio: f32,
    range: f32,
    /// Fixed release in ms, `None` for Auto.
    release: Option<f32>,
    external: bool,
    listen: bool,
    clip_on: bool,
    hpf_on: bool,
    hpf: SvfCoeffs,
    hpf_st: [SvfState; 2],
    bus: BusGain,
    makeup: Smoother,
    mix: Smoother,
    output: Smoother,
    /// 0 = plain path, 1 = soft-clipped (delayed) path; fades in 5 ms.
    clip_fade: LinearRamp,
    /// 0 = normal output, 1 = the filtered key; fades in 5 ms.
    listen_fade: LinearRamp,
    os: [Oversampler; 2],
    shapers: [Adaa1; 2],
    dry: [DelayLine; 2],
    /// Latency of the soft clip stage (samples), applied when it is on.
    clip_latency: usize,
    gr_disp: f32,
    in_peak: f32,
    out_peak: f32,
    clip_hold: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(BusComp::new())
}

impl Default for BusComp {
    fn default() -> Self {
        Self::new()
    }
}

impl BusComp {
    pub fn new() -> Self {
        let params = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Bus Compressor",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "compressor", "bus", "glue", "sidechain"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: true,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            thr: -12.0,
            ratio: 4.0,
            range: 40.0,
            release: None,
            external: false,
            listen: false,
            clip_on: false,
            hpf_on: false,
            hpf: SvfCoeffs::default(),
            hpf_st: [SvfState::default(); 2],
            bus: BusGain::new(),
            makeup: Smoother::new(1.0),
            mix: Smoother::new(1.0),
            output: Smoother::new(1.0),
            clip_fade: LinearRamp::new(0.0),
            listen_fade: LinearRamp::new(0.0),
            os: [Oversampler::new(2); 2],
            shapers: [Adaa1::new(ShaperKind::Tanh); 2],
            dry: [DelayLine::new(1), DelayLine::new(1)],
            clip_latency: 0,
            gr_disp: 0.0,
            in_peak: 0.0,
            out_peak: 0.0,
            clip_hold: 0.0,
            first: true,
        };
        c.prepare(48_000.0, 512);
        c
    }

    /// Static curve: output level (dB) for an input level (dB), makeup included.
    pub fn static_curve(&self, in_db: f32) -> f32 {
        let ps = &self.params;
        let ratio = RATIOS[ps.choice(p::RATIO).min(2)];
        let gr = gain_computer(in_db, ps.plain(p::THRESHOLD), ratio, KNEE_DB).min(ps.plain(p::RANGE));
        in_db - gr + ps.plain(p::MAKEUP)
    }

    fn update(&mut self) {
        let ps = &self.params;
        let sr = self.sr;
        self.thr = ps.plain(p::THRESHOLD);
        self.ratio = RATIOS[ps.choice(p::RATIO).min(2)];
        self.range = ps.plain(p::RANGE);
        self.bus.set_attack_ms(ATTACKS_MS[ps.choice(p::ATTACK).min(6)], sr);
        let rel = ps.choice(p::RELEASE).min(RELEASE_AUTO);
        self.release = RELEASES_MS.get(rel).copied();
        self.bus.set_release(self.release, sr);
        self.external = ps.on(p::SIDECHAIN);
        self.listen = ps.on(p::SC_LISTEN);
        let hz = SC_HPF_HZ[ps.choice(p::SC_HPF).min(6)];
        self.hpf_on = hz > 0.0;
        if self.hpf_on {
            self.hpf = SvfCoeffs::new(SvfMode::Highpass, hz, std::f32::consts::FRAC_1_SQRT_2, 0.0, sr);
        }
        let clip = ps.on(p::SOFT_CLIP);
        if clip != self.clip_on {
            self.clip_on = clip;
            if clip {
                // Cold start: the fade brings the stage in from nothing.
                for o in &mut self.os {
                    o.reset();
                }
                for s in &mut self.shapers {
                    s.reset();
                }
            }
        }
        self.clip_fade.set_target(if clip { 1.0 } else { 0.0 });
        self.listen_fade.set_target(if self.listen { 1.0 } else { 0.0 });
        self.makeup.set_target(db_to_gain(ps.plain(p::MAKEUP)));
        self.mix.set_target(ps.plain(p::MIX) / 100.0);
        self.output.set_target(db_to_gain(ps.plain(p::OUTPUT)));
        if self.first {
            self.clip_fade.snap(if clip { 1.0 } else { 0.0 });
            self.listen_fade.snap(if self.listen { 1.0 } else { 0.0 });
            self.makeup.snap_to_target();
            self.mix.snap_to_target();
            self.output.snap_to_target();
            self.first = false;
        }
    }
}

impl Segmented for BusComp {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let sc = if self.external { ctx.sidechain } else { None };
        let external = self.external;
        let (thr, ratio, range) = (self.thr, self.ratio, self.range);
        let lat = self.clip_latency;
        let ceil = db_to_gain(CEILING_DB);
        let inv_ceil = 1.0 / ceil;
        let mut in_peak = self.in_peak * 0.5;
        let mut out_peak = self.out_peak * 0.5;
        let mut gr_max = 0.0f32;
        let mut clip_peak = 0.0f32;
        let this = &mut *self;
        for_frames(ctx.buffer, start, end, |n, l, r| {
            let (mut dl, mut dr) = if external { sidechain_frame(sc, n) } else { (l, r) };
            if this.hpf_on {
                dl = this.hpf_st[0].process(&this.hpf, dl);
                dr = this.hpf_st[1].process(&this.hpf, dr);
            }
            let lf = this.listen_fade.next();
            let gr = this.bus.process(dl, dr, thr, ratio, range);
            gr_max = gr_max.max(gr);
            in_peak = in_peak.max(l.abs()).max(r.abs());
            let g = db_to_gain(-gr) * this.makeup.next();
            let mut wet = [l * g, r * g];
            let cf = this.clip_fade.next();
            if cf > 0.0 {
                for c in 0..2 {
                    let mut up = [0.0f32; 4];
                    this.os[c].upsample(wet[c], &mut up);
                    for v in up.iter_mut().take(2) {
                        let u = *v * inv_ceil;
                        clip_peak = clip_peak.max(u.abs());
                        *v = this.shapers[c].process(u) * ceil;
                    }
                    let clipped = this.os[c].downsample(&up);
                    wet[c] += (clipped - wet[c]) * cf;
                }
            }
            this.dry[0].push(l);
            this.dry[1].push(r);
            let mut dry = [l, r];
            if cf > 0.0 {
                dry[0] += (this.dry[0].read(lat) - l) * cf;
                dry[1] += (this.dry[1].read(lat) - r) * cf;
            }
            let mix = this.mix.next();
            let out_g = this.output.next();
            let mut ol = (dry[0] + (wet[0] - dry[0]) * mix) * out_g;
            let mut or = (dry[1] + (wet[1] - dry[1]) * mix) * out_g;
            out_peak = out_peak.max(ol.abs()).max(or.abs());
            if lf > 0.0 {
                ol += (dl - ol) * lf;
                or += (dr - or) * lf;
            }
            (sanitize(ol), sanitize(or))
        });
        let secs = (end - start) as f32 / self.sr as f32;
        self.in_peak = in_peak;
        self.out_peak = out_peak;
        // The clip works hard once the tanh takes ≥ 0.5 dB off a sample.
        self.clip_hold = if clip_peak > 0.5 { 0.25 } else { (self.clip_hold - secs).max(0.0) };
        self.gr_disp = gr_max.max(self.gr_disp - GR_FALL_DB_S * secs).max(0.0);
    }
}

impl Processor for BusComp {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.bus.prepare(sample_rate);
        self.makeup.set_time(20.0, sample_rate);
        self.mix.set_time(20.0, sample_rate);
        self.output.set_time(20.0, sample_rate);
        self.clip_fade.set_time(5.0, sample_rate);
        self.listen_fade.set_time(5.0, sample_rate);
        self.os = [Oversampler::new(2); 2];
        // Round trip of the 2× stage plus half a high-rate sample of ADAA.
        self.clip_latency = (Oversampler::new(2).latency() + 0.25).round() as usize;
        self.dry = [DelayLine::new(64), DelayLine::new(64)];
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.bus.reset();
        for s in &mut self.hpf_st {
            s.reset();
        }
        for o in &mut self.os {
            o.reset();
        }
        for s in &mut self.shapers {
            s.reset();
        }
        for d in &mut self.dry {
            d.clear();
        }
        self.gr_disp = 0.0;
        self.in_peak = 0.0;
        self.out_peak = 0.0;
        self.clip_hold = 0.0;
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
        if self.params.on(p::SOFT_CLIP) { self.clip_latency as u32 } else { 0 }
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        let mut o = write_meter(out, 0, &[-self.gr_disp, gain_to_db(self.in_peak), gain_to_db(self.out_peak)]);
        let mut curve = [0.0f32; 64];
        for (i, c) in curve.iter_mut().enumerate() {
            *c = self.static_curve(-60.0 + 60.0 * i as f32 / 63.0);
        }
        o = write_meter(out, o, &curve);
        write_meter(out, o, &[self.latency() as f32, if self.clip_hold > 0.0 { 1.0 } else { 0.0 }, self.bus.release_ms(self.release)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{BLOCK, SR, peak, process_stereo, rms, set_plain, sine};
    use aitrack_dsp::{AudioBuffer, Transport};

    fn level_db(x: &[f32]) -> f32 {
        gain_to_db(peak(x))
    }

    /// Amplitude (peak) of a tone stepping through `levels` dBFS, `seg`
    /// samples each.
    fn stepped(freq: f32, levels: &[f32], seg: usize) -> impl FnMut(usize) -> (f32, f32) + '_ {
        move |i| {
            let amp = db_to_gain(levels[(i / seg).min(levels.len() - 1)]);
            let v = amp * (std::f64::consts::TAU * freq as f64 * i as f64 / SR).sin() as f32;
            (v, v)
        }
    }

    /// Steady-state gain reduction (dB) in the last 100 ms of each segment.
    fn reductions(c: &mut BusComp, levels: &[f32], seg: usize) -> Vec<f32> {
        let (l, _) = process_stereo(c, stepped(1000.0, levels, seg), seg * levels.len());
        levels
            .iter()
            .enumerate()
            .map(|(k, lv)| {
                let end = (k + 1) * seg;
                lv - level_db(&l[end - 4_800..end])
            })
            .collect()
    }

    fn theory(level: f32, thr: f32, ratio: f32) -> f32 {
        gain_computer(level, thr, ratio, KNEE_DB)
    }

    #[test]
    fn reduction_on_a_stepped_tone_matches_the_theoretical_curve() {
        // Up and down: the levels cross the knee, the straight part and 0 dBFS.
        let levels = [-40.0f32, -30.0, -22.0, -16.0, -12.0, -8.0, -4.0, -1.0, -8.0, -16.0, -30.0];
        let mut worst = 0.0f32;
        for (ri, ratio) in RATIOS.iter().enumerate() {
            for thr in [-24.0f32, -12.0] {
                for att in [0usize, 3, 6] {
                    let mut c = BusComp::new();
                    set_plain(&mut c, p::THRESHOLD, thr);
                    set_plain(&mut c, p::RATIO, ri as f32);
                    set_plain(&mut c, p::ATTACK, att as f32);
                    set_plain(&mut c, p::RELEASE, 0.0); // 0.1 s: settles inside a segment
                    let got = reductions(&mut c, &levels, 36_000);
                    for (k, lv) in levels.iter().enumerate() {
                        let want = theory(*lv, thr, *ratio);
                        worst = worst.max((got[k] - want).abs());
                        assert!((got[k] - want).abs() < 0.5, "ratio {ratio} thr {thr} attack {} level {lv}: GR {} want {want}", ATTACKS_MS[att], got[k]);
                    }
                }
            }
        }
        eprintln!("worst deviation from the theoretical curve: {worst:.3} dB");
    }

    #[test]
    fn auto_release_reaches_the_same_steady_reduction() {
        let levels = [-30.0f32, -18.0, -9.0, -3.0];
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -24.0);
        set_plain(&mut c, p::RATIO, 2.0);
        set_plain(&mut c, p::RELEASE, RELEASE_AUTO as f32);
        let got = reductions(&mut c, &levels, 48_000);
        for (k, lv) in levels.iter().enumerate() {
            let want = theory(*lv, -24.0, 10.0);
            assert!((got[k] - want).abs() < 0.5, "level {lv}: GR {} want {want}", got[k]);
        }
    }

    #[test]
    fn static_curve_matches_the_gain_computer() {
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -20.0);
        set_plain(&mut c, p::RATIO, 1.0);
        set_plain(&mut c, p::MAKEUP, 3.0);
        let mut meter = [0.0f32; 128];
        assert_eq!(c.meter(&mut meter), 70);
        // Hard part of the curve: 20 dB over → 15 dB reduction at 4:1.
        let want = -0.0 - 15.0 + 3.0;
        assert!((c.static_curve(0.0) - want).abs() < 1e-3);
        assert!((meter[3 + 63] - want).abs() < 1e-3, "{}", meter[66]);
        assert!(meter[3] > -60.1 && meter[3] < -56.0, "{}", meter[3]);
        assert!(meter[..70].iter().all(|v| v.is_finite()));
    }

    #[test]
    fn latency_appears_only_with_the_soft_clip() {
        let mut c = BusComp::new();
        c.prepare(SR, BLOCK);
        assert_eq!(c.latency(), 0);
        set_plain(&mut c, p::SOFT_CLIP, 1.0);
        assert!(c.latency() > 0 && c.latency() < 32, "{}", c.latency());
        let mut meter = [0.0f32; 80];
        c.meter(&mut meter);
        assert_eq!(meter[67], c.latency() as f32);
        set_plain(&mut c, p::SOFT_CLIP, 0.0);
        assert_eq!(c.latency(), 0);
    }

    #[test]
    fn declared_latency_aligns_wet_and_dry() {
        // No compression (threshold 0, quiet tone): the output is the input
        // delayed by the reported latency, and Mix does not comb.
        let n = 24_000;
        let lat = {
            let mut c = BusComp::new();
            set_plain(&mut c, p::SOFT_CLIP, 1.0);
            c.latency() as usize
        };
        let input = |i: usize| {
            let v = 0.1 * (std::f64::consts::TAU * 220.0 * i as f64 / SR).sin() as f32;
            (v, v)
        };
        let render = |mix: f32| {
            let mut c = BusComp::new();
            set_plain(&mut c, p::THRESHOLD, 0.0);
            set_plain(&mut c, p::SOFT_CLIP, 1.0);
            set_plain(&mut c, p::MIX, mix);
            process_stereo(&mut c, input, n).0
        };
        let (wet, dry, half) = (render(100.0), render(0.0), render(50.0));
        for i in 4_000..n {
            let want = input(i - lat).0;
            assert!((dry[i] - want).abs() < 1e-5, "dry misaligned at {i}");
            assert!((wet[i] - want).abs() < 2e-3, "wet misaligned at {i}: {} vs {want}", wet[i]);
            assert!((half[i] - want).abs() < 2e-3, "mix misaligned at {i}");
        }
    }

    #[test]
    fn soft_clip_holds_the_ceiling_and_is_transparent_when_quiet() {
        let ceil = db_to_gain(CEILING_DB);
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, 0.0);
        set_plain(&mut c, p::SOFT_CLIP, 1.0);
        set_plain(&mut c, p::MAKEUP, 12.0);
        let (l, _) = process_stereo(&mut c, sine(1000.0, 1.0), 24_000);
        let pk = peak(&l[12_000..]);
        assert!(pk <= ceil * 1.02 && pk > ceil * 0.9, "clipped peak {pk}, ceiling {ceil}");
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, 0.0);
        set_plain(&mut c, p::SOFT_CLIP, 1.0);
        let (l, _) = process_stereo(&mut c, sine(1000.0, 0.02), 24_000);
        let ratio = peak(&l[12_000..]) / 0.02;
        assert!((ratio - 1.0).abs() < 0.01, "quiet gain {ratio}");
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, 0.0);
        set_plain(&mut c, p::SOFT_CLIP, 1.0);
        set_plain(&mut c, p::MAKEUP, 12.0);
        let mut meter = [0.0f32; 80];
        process_stereo(&mut c, sine(1000.0, 1.0), 2_400);
        c.meter(&mut meter);
        assert_eq!(meter[68], 1.0);
    }

    #[test]
    fn mix_zero_is_the_dry_signal() {
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -40.0);
        set_plain(&mut c, p::MIX, 0.0);
        set_plain(&mut c, p::MAKEUP, 12.0);
        let (l, _) = process_stereo(&mut c, sine(700.0, 0.5), 24_000);
        let tone: Vec<f32> = (0..24_000).map(|i| sine(700.0, 0.5)(i).0).collect();
        let err = l.iter().zip(&tone).skip(2_400).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(err < 1e-5, "{err}");
    }

    #[test]
    fn slow_attack_lets_the_transient_through() {
        let burst = |i: usize| {
            let v = if i < 4_800 { 0.0 } else { (std::f64::consts::TAU * 1000.0 * i as f64 / SR).sin() as f32 };
            (v, v)
        };
        let early_peak = |att: usize| {
            let mut c = BusComp::new();
            set_plain(&mut c, p::THRESHOLD, -30.0);
            set_plain(&mut c, p::RATIO, 2.0);
            set_plain(&mut c, p::ATTACK, att as f32);
            let (l, _) = process_stereo(&mut c, burst, 4_800 + 9_600);
            // First 3 ms of the burst.
            peak(&l[4_800..4_800 + 144])
        };
        let fast = early_peak(0);
        let slow = early_peak(6);
        assert!(slow > 0.9, "30 ms attack let {slow} through");
        assert!(fast < 0.6, "0.01 ms attack let {fast} through");
    }

    #[test]
    fn auto_release_recovers_slower_after_sustained_reduction() {
        let recovery = |release: usize, hold_s: f32| {
            let hold = (hold_s * SR as f32) as usize;
            let mut c = BusComp::new();
            set_plain(&mut c, p::THRESHOLD, -20.0);
            set_plain(&mut c, p::RATIO, 1.0);
            set_plain(&mut c, p::ATTACK, 3.0);
            set_plain(&mut c, p::RELEASE, release as f32);
            // 6 dB of reduction for `hold`, then a quiet probe tone below
            // the threshold.
            let input = move |i: usize| {
                let a = if i < hold { 0.25 } else { 0.01 };
                let v = a * (std::f64::consts::TAU * 1000.0 * i as f64 / SR).sin() as f32;
                (v, v)
            };
            let (l, _) = process_stereo(&mut c, input, hold + 24_000);
            // Probe gain 250 ms after the drop.
            let k = hold + 12_000;
            gain_to_db(peak(&l[k..k + 480]) / 0.01)
        };
        let fixed = recovery(0, 3.0);
        let auto_brief = recovery(RELEASE_AUTO, 0.1);
        let auto_long = recovery(RELEASE_AUTO, 3.0);
        assert!(fixed > -3.0, "0.1 s release still {fixed} dB at 250 ms");
        assert!(auto_brief > -3.0, "auto after a brief hit still {auto_brief} dB");
        assert!(auto_long < fixed - 2.0, "auto after a long hold {auto_long} dB vs fixed {fixed}");
    }

    #[test]
    fn sidechain_high_pass_ignores_the_low_end() {
        let measure = |hpf: usize| {
            let mut c = BusComp::new();
            set_plain(&mut c, p::THRESHOLD, -30.0);
            set_plain(&mut c, p::RATIO, 2.0);
            set_plain(&mut c, p::SC_HPF, hpf as f32);
            // A 30 Hz tone, two octaves under the highest corner.
            let (l, _) = process_stereo(&mut c, sine(30.0, 0.5), 48_000);
            level_db(&l[24_000..]) - level_db(&sine_buf(30.0, 0.5, 24_000))
        };
        let off = measure(0);
        let hp240 = measure(6);
        assert!(off < -6.0, "no filter: {off} dB");
        assert!(hp240 > -1.0, "240 Hz high-pass: {hp240} dB");
    }

    fn sine_buf(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        let mut f = sine(freq, amp);
        (0..n).map(|i| f(i).0).collect()
    }

    /// Run `n` frames with an external key.
    fn run_external(c: &mut BusComp, input: impl Fn(usize) -> f32, key: impl Fn(usize) -> f32, n: usize) -> Vec<f32> {
        c.prepare(SR, BLOCK);
        let mut buf = AudioBuffer::new(2, BLOCK);
        let mut sc = AudioBuffer::new(2, BLOCK);
        let tr = Transport::default();
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < n {
            let len = BLOCK.min(n - pos);
            buf.set_frames(len);
            sc.set_frames(len);
            for i in 0..len {
                for ch in 0..2 {
                    buf.channel_mut(ch)[i] = input(pos + i);
                    sc.channel_mut(ch)[i] = key(pos + i);
                }
            }
            let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: Some(&sc), events: &[], transport: &tr };
            c.process(&mut ctx);
            out.extend_from_slice(buf.channel(0));
            pos += len;
        }
        out
    }

    #[test]
    fn external_sidechain_ducks_the_input() {
        let tone = |i: usize| 0.1 * (std::f64::consts::TAU * 500.0 * i as f64 / SR).sin() as f32;
        let key = |i: usize| 0.9 * (std::f64::consts::TAU * 2000.0 * i as f64 / SR).sin() as f32;
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -12.0);
        set_plain(&mut c, p::RATIO, 2.0);
        set_plain(&mut c, p::SIDECHAIN, 1.0);
        let out = run_external(&mut c, tone, key, 48_000);
        let ducked = level_db(&out[36_000..]) - gain_to_db(0.1);
        // Key at −0.9 dBFS, threshold −12: ~11 dB over, 10:1 → ~10 dB.
        assert!(ducked < -8.0, "ducked only {ducked} dB");
        // Internal mode ignores the key: the quiet input stays untouched.
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -12.0);
        set_plain(&mut c, p::RATIO, 2.0);
        let out = run_external(&mut c, tone, key, 48_000);
        let untouched = level_db(&out[36_000..]) - gain_to_db(0.1);
        assert!(untouched.abs() < 0.2, "{untouched}");
    }

    #[test]
    fn listen_plays_the_filtered_key() {
        let mut c = BusComp::new();
        set_plain(&mut c, p::SC_LISTEN, 1.0);
        set_plain(&mut c, p::SC_HPF, 6.0);
        let (l, _) = process_stereo(&mut c, sine(40.0, 0.5), 48_000);
        assert!(rms(&l[24_000..]) < 0.02, "40 Hz through a 240 Hz high-pass: {}", rms(&l[24_000..]));
        let mut c = BusComp::new();
        set_plain(&mut c, p::SC_LISTEN, 1.0);
        set_plain(&mut c, p::SC_HPF, 6.0);
        let (l, _) = process_stereo(&mut c, sine(3000.0, 0.5), 48_000);
        assert!((peak(&l[24_000..]) - 0.5).abs() < 0.02);
    }

    #[test]
    fn range_caps_the_reduction() {
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -40.0);
        set_plain(&mut c, p::RATIO, 2.0);
        set_plain(&mut c, p::RANGE, 6.0);
        let got = reductions(&mut c, &[-1.0], 48_000)[0];
        assert!((got - 6.0).abs() < 0.3, "{got}");
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -40.0);
        set_plain(&mut c, p::RATIO, 2.0);
        set_plain(&mut c, p::RANGE, 0.0);
        assert!(reductions(&mut c, &[-1.0], 48_000)[0].abs() < 0.1);
    }

    #[test]
    fn the_detector_is_stereo_linked() {
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, -20.0);
        set_plain(&mut c, p::RATIO, 2.0);
        // Loud left, quiet right: both get the same gain.
        let input = |i: usize| {
            let s = (std::f64::consts::TAU * 1000.0 * i as f64 / SR).sin() as f32;
            (0.9 * s, 0.09 * s)
        };
        let (l, r) = process_stereo(&mut c, input, 48_000);
        let gl = level_db(&l[36_000..]) - gain_to_db(0.9);
        let gr = level_db(&r[36_000..]) - gain_to_db(0.09);
        assert!(gl < -6.0, "{gl}");
        assert!((gl - gr).abs() < 0.1, "left {gl} dB, right {gr} dB");
    }

    #[test]
    fn makeup_and_output_add_gain() {
        let mut c = BusComp::new();
        set_plain(&mut c, p::THRESHOLD, 0.0);
        set_plain(&mut c, p::MAKEUP, 6.0);
        set_plain(&mut c, p::OUTPUT, -2.0);
        let (l, _) = process_stereo(&mut c, sine(1000.0, 0.1), 24_000);
        let g = level_db(&l[12_000..]) - gain_to_db(0.1);
        assert!((g - 4.0).abs() < 0.1, "{g}");
    }

    #[test]
    fn state_round_trips() {
        let mut a = BusComp::new();
        set_plain(&mut a, p::THRESHOLD, -31.0);
        set_plain(&mut a, p::RATIO, 2.0);
        set_plain(&mut a, p::RELEASE, 3.0);
        let mut b = BusComp::new();
        b.load_state(&a.save_state());
        for info in params() {
            assert!((a.get_param(info.id) - b.get_param(info.id)).abs() < 1e-5, "{}", info.name);
        }
    }

    #[test]
    fn factory_presets_land_on_whole_steps() {
        let list = crate::presets::list(ID);
        assert!(list.len() >= 8, "{} presets", list.len());
        for r in list {
            let preset = crate::presets::load(&r.path).unwrap();
            let mut c = BusComp::new();
            assert_eq!(preset.apply(&mut c), params().len(), "{}", r.name);
            for info in params().iter().filter(|i| !i.choices.is_empty()) {
                let steps = info.max - info.min;
                let x = c.get_param(info.id) * steps;
                assert!((x - x.round()).abs() < 1e-3, "{}: {} = {x}", r.name, info.name);
            }
            assert_eq!(c.latency() > 0, preset.params["6"] > 0.5, "{}", r.name);
        }
    }

    /// Every stepped parameter changes by an event in mid-block while a
    /// 100 Hz sine plays: the largest step between contiguous samples must
    /// stay of the order of the clean signal's own.
    #[test]
    fn discrete_changes_do_not_click() {
        use crate::test_util::param_change_step;
        let setup = [(p::THRESHOLD, -24.0), (p::MAKEUP, 6.0), (p::RELEASE, 2.0)];
        let cases: [(u32, &str, f32, f32); 16] = [
            (p::RATIO, "ratio", 0.0, 2.0),
            (p::RATIO, "ratio", 2.0, 0.0),
            (p::ATTACK, "attack", 0.0, 6.0),
            (p::ATTACK, "attack", 6.0, 0.0),
            (p::RELEASE, "release", 0.0, 6.0),
            (p::RELEASE, "release", 6.0, 0.0),
            (p::SIDECHAIN, "sidechain", 0.0, 1.0),
            (p::SIDECHAIN, "sidechain", 1.0, 0.0),
            (p::SC_HPF, "sc hpf", 0.0, 6.0),
            (p::SC_HPF, "sc hpf", 6.0, 0.0),
            (p::SOFT_CLIP, "soft clip on", 0.0, 1.0),
            (p::SOFT_CLIP, "soft clip off", 1.0, 0.0),
            (p::SC_LISTEN, "listen on", 0.0, 1.0),
            (p::SC_LISTEN, "listen off", 1.0, 0.0),
            (p::SC_HPF, "sc hpf 30", 6.0, 1.0),
            (p::RATIO, "ratio 4", 0.0, 1.0),
        ];
        let mut bad = Vec::new();
        for (id, name, from, to) in cases {
            let mut setup = setup.to_vec();
            if id == p::SC_LISTEN {
                setup.push((p::SC_HPF, 0.0));
            }
            let (ev, reference) = param_change_step(create, &setup, id, from, to, 0.8);
            eprintln!("{name:<14} step {ev:.4} (clean {reference:.4})");
            if ev > 2.0 * reference + 0.005 {
                bad.push(format!("{name}: step {ev:.4} against {reference:.4}"));
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }
}
