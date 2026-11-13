//! `fx.compressor` — compressor after Giannoulis, Massberg & Reiss (JAES
//! 2012): log-domain gain computer with a quadratic soft knee and smooth
//! peak detectors, in feed-forward or feedback topology.
//!
//! Styles (behavioural models, not emulations of any one unit):
//! * **Clean** — smooth decoupled detector, times as set.
//! * **Punch** — branching detector, attack ×1.6 and release ×0.6 so
//!   transients poke through and the body pumps.
//! * **Glue** — at least 60 % RMS detection and program-dependent release
//!   (fast for transients, slow for sustained reduction).
//! * **Opto** — attack ×2, knee ≥ 6 dB and a release that slows as the
//!   reduction history grows (two-stage memory).
//! * **VCA** — pure peak detection, branching attack and a *linear-in-dB*
//!   release (a constant 20 dB per release time, like classic VCA units
//!   rated in dB/s): fast, firm, the same recovery speed at any depth.
//! * **Vari-Mu** — the ratio grows with level (knee ≥ 18 dB), attack ×1.5,
//!   ≥ 50 % RMS and program-dependent release: thick, slow, glue-like.
//!
//! The style is `Style` (Clean / Punch / Glue / Opto) while `Model` is
//! Classic; `Model` = VCA or Vari-Mu selects those (two ids so presets made
//! before the new models load unchanged).
//!
//! Topology: **feed-forward** detects the input (or the sidechain);
//! **feedback** detects the compressor's own output (the previous sample),
//! the way many classic units are wired. Its gain computer has slope
//! `ratio − 1` above the threshold, so the steady-state curve is the same as
//! feed-forward; the closed loop speeds the attack up by about the ratio and
//! lets go at the set release once the output falls under the threshold,
//! which is what makes feedback compressors sound softer. The smoothing
//! coefficients are limited to `≤ 1 / ratio` per sample, which keeps the
//! discrete loop stable at any setting. An external sidechain always runs
//! feed-forward (the key is not the output).
//!
//! Lookahead delays the audio by the lookahead time; `latency()` reports the
//! current lookahead in samples (changing it changes the reported latency,
//! so hosts re-run delay compensation). Sidechain: internal or the external
//! sidechain bus, through a 12 dB/oct high-pass (off at 20 Hz), a bell
//! (`SC EQ` freq / gain, Q 1; off at 0 dB) and a 12 dB/oct low-pass (off at
//! 20 kHz). `SC Listen` plays the filtered detector signal.
//!
//! Meter layout (70 values): `[0]` gain reduction in dB (≤ 0, peak-held with
//! a 40 dB/s fall so the history misses no hit), `[1]` input peak dBFS, `[2]`
//! output peak dBFS, `[3..67)` static curve: output level in dB for inputs
//! from −60 to 0 dB in 64 steps (makeup included), `[67]` latency in samples,
//! `[68]` the style in effect (0 Clean … 5 Vari-Mu), `[69]` 1 when the
//! feedback topology is in effect.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, sidechain_frame, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::envelope::{GainSmoother, gain_computer};
use aitrack_dsp::smooth::{Smoother, one_pole_coeff};
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.compressor";

pub mod p {
    pub const THRESHOLD: u32 = 0;
    pub const RATIO: u32 = 1;
    pub const KNEE: u32 = 2;
    pub const ATTACK: u32 = 3;
    pub const RELEASE: u32 = 4;
    pub const AUTO_RELEASE: u32 = 5;
    pub const STYLE: u32 = 6;
    pub const RMS: u32 = 7;
    pub const LINK: u32 = 8;
    pub const LOOKAHEAD: u32 = 9;
    pub const SIDECHAIN: u32 = 10;
    pub const SC_HPF: u32 = 11;
    pub const RANGE: u32 = 12;
    pub const MAKEUP: u32 = 13;
    pub const AUTO_MAKEUP: u32 = 14;
    pub const MIX: u32 = 15;
    pub const OUTPUT: u32 = 16;
    pub const SC_LISTEN: u32 = 17;
    /// Classic (use `STYLE`) / VCA / Vari-Mu.
    pub const MODEL: u32 = 18;
    /// Feed-Forward / Feedback.
    pub const TOPOLOGY: u32 = 19;
    pub const SC_EQ_FREQ: u32 = 20;
    pub const SC_EQ_GAIN: u32 = 21;
    pub const SC_LPF: u32 = 22;
}

/// `STYLE` values.
pub mod style {
    pub const CLEAN: f32 = 0.0;
    pub const PUNCH: f32 = 1.0;
    pub const GLUE: f32 = 2.0;
    pub const OPTO: f32 = 3.0;
}

/// `MODEL` values.
pub mod model {
    pub const CLASSIC: f32 = 0.0;
    pub const VCA: f32 = 1.0;
    pub const VARI_MU: f32 = 2.0;
}

/// `TOPOLOGY` values.
pub mod topology {
    pub const FEED_FORWARD: f32 = 0.0;
    pub const FEEDBACK: f32 = 1.0;
}

/// The six styles in effect, as [`resolved_style`] numbers them.
pub const STYLE_NAMES: [&str; 6] = ["Clean", "Punch", "Glue", "Opto", "VCA", "Vari-Mu"];
const VCA: usize = 4;
const VARI_MU: usize = 5;

const OFF_ON: &[&str] = &["Off", "On"];
const STYLES: &[&str] = &["Clean", "Punch", "Glue", "Opto"];
const MODELS: &[&str] = &["Classic", "VCA", "Vari-Mu", "—"];
const TOPOLOGIES: &[&str] = &["Feed-Forward", "Feedback"];
const SC: &[&str] = &["Internal", "External"];
const MAX_LOOKAHEAD_MS: f32 = 10.0;
const CONTROL: usize = 16;
/// Fall of the gain-reduction meter, dB per second.
const GR_FALL_DB_S: f32 = 40.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::THRESHOLD, "Threshold", -60.0, 0.0, -18.0).unit("dB").short("Thresh"),
        ParamInfo::new(p::RATIO, "Ratio", 1.0, 100.0, 4.0).log(),
        ParamInfo::new(p::KNEE, "Knee", 0.0, 24.0, 6.0).unit("dB"),
        ParamInfo::new(p::ATTACK, "Attack", 0.01, 200.0, 10.0).unit("ms").log().short("Att"),
        ParamInfo::new(p::RELEASE, "Release", 5.0, 3_000.0, 120.0).unit("ms").log().short("Rel"),
        ParamInfo::new(p::AUTO_RELEASE, "Auto Release", 0.0, 1.0, 0.0).choices(OFF_ON).short("Auto Rel"),
        ParamInfo::new(p::STYLE, "Style", 0.0, 3.0, style::CLEAN).choices(STYLES),
        ParamInfo::new(p::RMS, "RMS Detection", 0.0, 100.0, 0.0).unit("%").short("RMS"),
        ParamInfo::new(p::LINK, "Stereo Link", 0.0, 100.0, 100.0).unit("%").short("Link"),
        ParamInfo::new(p::LOOKAHEAD, "Lookahead", 0.0, MAX_LOOKAHEAD_MS, 0.0).unit("ms").short("Look"),
        ParamInfo::new(p::SIDECHAIN, "Sidechain", 0.0, 1.0, 0.0).choices(SC).short("SC"),
        ParamInfo::new(p::SC_HPF, "SC High-Pass", 20.0, 500.0, 20.0).unit("Hz").log().short("SC HPF"),
        ParamInfo::new(p::RANGE, "Range", 0.0, 60.0, 60.0).unit("dB"),
        ParamInfo::new(p::MAKEUP, "Makeup", -12.0, 24.0, 0.0).unit("dB"),
        ParamInfo::new(p::AUTO_MAKEUP, "Auto Makeup", 0.0, 1.0, 0.0).choices(OFF_ON).short("Auto"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 24.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::SC_LISTEN, "SC Listen", 0.0, 1.0, 0.0).choices(OFF_ON).short("Listen"),
        ParamInfo::new(p::MODEL, "Model", 0.0, 3.0, model::CLASSIC).choices(MODELS),
        ParamInfo::new(p::TOPOLOGY, "Topology", 0.0, 1.0, topology::FEED_FORWARD).choices(TOPOLOGIES).short("Topo"),
        ParamInfo::new(p::SC_EQ_FREQ, "SC EQ Freq", 40.0, 12_000.0, 1_000.0).unit("Hz").log().short("SC Freq"),
        ParamInfo::new(p::SC_EQ_GAIN, "SC EQ Gain", -18.0, 18.0, 0.0).unit("dB").short("SC Gain"),
        ParamInfo::new(p::SC_LPF, "SC Low-Pass", 1_000.0, 20_000.0, 20_000.0).unit("Hz").log().short("SC LPF"),
    ]
}

/// The style in effect (0 Clean, 1 Punch, 2 Glue, 3 Opto, 4 VCA, 5 Vari-Mu)
/// from the `STYLE` and `MODEL` indices.
pub fn resolved_style(style: usize, model: usize) -> usize {
    match model {
        1 => VCA,
        2 => VARI_MU,
        _ => style.min(3),
    }
}

/// The knee a style actually runs: Opto never goes under 6 dB, Vari-Mu's
/// ratio grows over at least 18 dB.
pub fn effective_knee(style: usize, knee: f32) -> f32 {
    match style {
        3 => knee.max(6.0),
        VARI_MU => knee.max(18.0),
        _ => knee,
    }
}

/// Feedback gain computer: slope `ratio − 1` above the threshold (same
/// quadratic knee shape), so `y − T = (x − T) / ratio` in steady state.
#[inline]
pub fn feedback_gain(level_db: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
    2.0 * (ratio - 1.0).max(0.0) * gain_computer(level_db, threshold, 2.0, knee)
}

/// Shortest smoothing time (ms) that keeps a feedback loop of `ratio`
/// stable: the per-sample coefficient must stay ≤ `1 / ratio`.
pub fn feedback_min_ms(ratio: f32, sample_rate: f64) -> f32 {
    let r = ratio.max(1.0001) as f64;
    let n = -1.0 / (1.0 - 1.0 / r).ln();
    (n / (0.001 * sample_rate)) as f32
}

pub struct Compressor {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    // Resolved settings.
    thr: f32,
    ratio: f32,
    knee: f32,
    range: f32,
    attack_ms: f32,
    release_ms: f32,
    style: usize,
    auto_release: bool,
    rms_mix: f32,
    link: f32,
    external: bool,
    feedback: bool,
    listen: bool,
    hpf_on: bool,
    hpf: SvfCoeffs,
    hpf_st: [SvfState; 2],
    bell_on: bool,
    bell: SvfCoeffs,
    bell_st: [SvfState; 2],
    lpf_on: bool,
    lpf: SvfCoeffs,
    lpf_st: [SvfState; 2],
    lookahead: usize,
    delay: [DelayLine; 2],
    rms_coeff: f32,
    ms: [f32; 2],
    det: [GainSmoother; 2],
    /// VCA ballistics: state, attack coefficient, release in dB per sample.
    vca: [f32; 2],
    vca_att: f32,
    vca_rate: f32,
    /// Feedback: the previous wet output per channel.
    fb_y: [f32; 2],
    gr_slow: f32,
    slow_coeff: f32,
    makeup: Smoother,
    mix: Smoother,
    gr_now: f32,
    gr_disp: f32,
    in_peak: f32,
    out_peak: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Compressor::new())
}

impl Default for Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor {
    pub fn new() -> Self {
        let params = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Compressor",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "compressor", "sidechain"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: true,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            thr: -18.0,
            ratio: 4.0,
            knee: 6.0,
            range: 60.0,
            attack_ms: 10.0,
            release_ms: 120.0,
            style: 0,
            auto_release: false,
            rms_mix: 0.0,
            link: 1.0,
            external: false,
            feedback: false,
            listen: false,
            hpf_on: false,
            hpf: SvfCoeffs::default(),
            hpf_st: [SvfState::default(); 2],
            bell_on: false,
            bell: SvfCoeffs::passthrough(),
            bell_st: [SvfState::default(); 2],
            lpf_on: false,
            lpf: SvfCoeffs::passthrough(),
            lpf_st: [SvfState::default(); 2],
            lookahead: 0,
            delay: [DelayLine::new(1), DelayLine::new(1)],
            rms_coeff: 0.01,
            ms: [0.0; 2],
            det: [GainSmoother::default(); 2],
            vca: [0.0; 2],
            vca_att: 1.0,
            vca_rate: 0.0,
            fb_y: [0.0; 2],
            gr_slow: 0.0,
            slow_coeff: 0.0,
            makeup: Smoother::new(1.0),
            mix: Smoother::new(1.0),
            gr_now: 0.0,
            gr_disp: 0.0,
            in_peak: 0.0,
            out_peak: 0.0,
            first: true,
        };
        c.prepare(48_000.0, 512);
        c
    }

    /// Static curve: output level (dB) for an input level (dB), makeup included.
    pub fn static_curve(&self, in_db: f32) -> f32 {
        let gr = gain_computer(in_db, self.thr, self.ratio, self.effective_knee()).min(self.range);
        in_db - gr + gain_to_db(self.makeup.target())
    }

    fn effective_knee(&self) -> f32 {
        effective_knee(self.style, self.knee)
    }

    fn update(&mut self) {
        let ps = &self.params;
        self.thr = ps.plain(p::THRESHOLD);
        self.ratio = ps.plain(p::RATIO);
        self.knee = ps.plain(p::KNEE);
        self.range = ps.plain(p::RANGE);
        self.attack_ms = ps.plain(p::ATTACK);
        self.release_ms = ps.plain(p::RELEASE);
        self.style = resolved_style(ps.choice(p::STYLE), ps.choice(p::MODEL));
        self.auto_release = ps.on(p::AUTO_RELEASE) || self.style == 2 || self.style == VARI_MU;
        self.rms_mix = ps.plain(p::RMS) / 100.0;
        match self.style {
            2 => self.rms_mix = self.rms_mix.max(0.6),
            VARI_MU => self.rms_mix = self.rms_mix.max(0.5),
            VCA => self.rms_mix = 0.0,
            _ => {}
        }
        self.link = ps.plain(p::LINK) / 100.0;
        self.external = ps.choice(p::SIDECHAIN) == 1;
        let feedback = ps.choice(p::TOPOLOGY) == 1 && !self.external;
        if feedback != self.feedback {
            self.fb_y = [0.0; 2];
        }
        self.feedback = feedback;
        self.listen = ps.on(p::SC_LISTEN);
        let hpf = ps.plain(p::SC_HPF);
        self.hpf_on = hpf > 20.5;
        self.hpf = SvfCoeffs::new(SvfMode::Highpass, hpf, std::f32::consts::FRAC_1_SQRT_2, 0.0, self.sr);
        let bell_gain = ps.plain(p::SC_EQ_GAIN);
        self.bell_on = bell_gain.abs() > 0.05;
        self.bell = SvfCoeffs::new(SvfMode::Bell, ps.plain(p::SC_EQ_FREQ), 1.0, bell_gain, self.sr);
        let lpf = ps.plain(p::SC_LPF);
        self.lpf_on = lpf < 19_500.0;
        self.lpf = SvfCoeffs::new(SvfMode::Lowpass, lpf.min(self.sr as f32 * 0.45), std::f32::consts::FRAC_1_SQRT_2, 0.0, self.sr);
        let la = ((ps.plain(p::LOOKAHEAD) * 0.001) as f64 * self.sr).round() as usize;
        self.lookahead = la.min(self.delay[0].max_delay());
        let mut makeup = ps.plain(p::MAKEUP);
        if ps.on(p::AUTO_MAKEUP) {
            // Half the static reduction a 0 dBFS signal would get.
            makeup += 0.5 * gain_computer(0.0, self.thr, self.ratio, self.effective_knee()).min(self.range);
        }
        self.makeup.set_target(db_to_gain(makeup + ps.plain(p::OUTPUT)));
        self.mix.set_target(ps.plain(p::MIX) / 100.0);
        self.set_times(self.release_ms);
        if self.first {
            self.makeup.snap_to_target();
            self.mix.snap_to_target();
            self.first = false;
        }
    }

    fn set_times(&mut self, release_ms: f32) {
        let (mut a, mut r) = match self.style {
            1 => (self.attack_ms * 1.6, release_ms * 0.6),
            3 => (self.attack_ms * 2.0, release_ms),
            VARI_MU => (self.attack_ms * 1.5, release_ms),
            _ => (self.attack_ms, release_ms),
        };
        if self.feedback {
            let min = feedback_min_ms(self.ratio, self.sr);
            a = a.max(min);
            r = r.max(min);
        }
        for d in &mut self.det {
            d.set_times(a, r, self.sr);
        }
        self.vca_att = one_pole_coeff(a, self.sr);
        // 20 dB per release time.
        self.vca_rate = 20.0 / (r.max(0.1) * 0.001 * self.sr as f32);
    }

    /// Program-dependent release at control rate.
    fn control(&mut self) {
        let gr = self.gr_now;
        if self.style == 3 {
            // Opto: the longer and deeper the reduction, the slower it lets go.
            let memory = (self.gr_slow / 6.0).clamp(0.0, 1.0);
            self.set_times(self.release_ms * (1.0 + 3.0 * memory));
        } else if self.auto_release {
            // Transient GR (well above its average) recovers fast, sustained
            // GR slowly.
            let sustain = if gr > 0.1 { (self.gr_slow / gr).clamp(0.0, 1.0) } else { 1.0 };
            self.set_times(self.release_ms * (0.35 + 1.65 * sustain));
        }
    }
}

impl Segmented for Compressor {
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
        let feedback = self.feedback;
        let (thr, ratio, knee, range) = (self.thr, self.ratio, self.effective_knee(), self.range);
        let branching = self.style == 1;
        let vca = self.style == VCA;
        let la = self.lookahead;
        let mut in_peak = self.in_peak * 0.5;
        let mut out_peak = self.out_peak * 0.5;
        let mut gr_max = 0.0f32;
        let mut i = start;
        while i < end {
            let e = (i + CONTROL).min(end);
            if self.auto_release || self.style == 3 {
                self.control();
            }
            let this = &mut *self;
            for_frames(ctx.buffer, i, e, |n, l, r| {
                let (mut dl, mut dr) = if external {
                    sidechain_frame(sc, n)
                } else if feedback {
                    (this.fb_y[0], this.fb_y[1])
                } else {
                    (l, r)
                };
                if this.hpf_on {
                    dl = this.hpf_st[0].process(&this.hpf, dl);
                    dr = this.hpf_st[1].process(&this.hpf, dr);
                }
                if this.bell_on {
                    dl = this.bell_st[0].process(&this.bell, dl);
                    dr = this.bell_st[1].process(&this.bell, dr);
                }
                if this.lpf_on {
                    dl = this.lpf_st[0].process(&this.lpf, dl);
                    dr = this.lpf_st[1].process(&this.lpf, dr);
                }
                // Peak / RMS blend per channel.
                let mut lvl = [0.0f32; 2];
                for (c, x) in [dl, dr].into_iter().enumerate() {
                    this.ms[c] += (x * x - this.ms[c]) * this.rms_coeff;
                    if this.ms[c] < 1e-30 {
                        this.ms[c] = 0.0;
                    }
                    let rms = (2.0 * this.ms[c]).sqrt(); // sine RMS → peak scale
                    let v = x.abs() + (rms - x.abs()) * this.rms_mix;
                    lvl[c] = gain_to_db(v);
                }
                let linked = lvl[0].max(lvl[1]);
                let mut gr = [0.0f32; 2];
                for c in 0..2 {
                    let d = lvl[c] + (linked - lvl[c]) * this.link;
                    let target = if feedback { feedback_gain(d, thr, ratio, knee) } else { gain_computer(d, thr, ratio, knee) }.min(range);
                    gr[c] = if vca {
                        let y = &mut this.vca[c];
                        if target > *y {
                            *y += (target - *y) * this.vca_att;
                        } else {
                            *y = (*y - this.vca_rate).max(target);
                        }
                        if *y < 1e-9 {
                            *y = 0.0;
                        }
                        *y
                    } else if branching {
                        this.det[c].process_branching(target)
                    } else {
                        this.det[c].process(target)
                    };
                }
                let g_now = gr[0].max(gr[1]);
                this.gr_slow += (g_now - this.gr_slow) * this.slow_coeff;
                this.gr_now = g_now;
                gr_max = gr_max.max(g_now);
                // Main path (delayed by the lookahead).
                this.delay[0].push(l);
                this.delay[1].push(r);
                let (xl, xr) = (this.delay[0].read(la), this.delay[1].read(la));
                in_peak = in_peak.max(xl.abs()).max(xr.abs());
                let wl = xl * db_to_gain(-gr[0]);
                let wr = xr * db_to_gain(-gr[1]);
                this.fb_y = [sanitize(wl), sanitize(wr)];
                if this.listen {
                    return (sanitize(dl), sanitize(dr));
                }
                let mk = this.makeup.next();
                let mix = this.mix.next();
                let ol = xl + (wl * mk - xl) * mix;
                let or = xr + (wr * mk - xr) * mix;
                out_peak = out_peak.max(ol.abs()).max(or.abs());
                (sanitize(ol), sanitize(or))
            });
            i = e;
        }
        self.in_peak = in_peak;
        self.out_peak = out_peak;
        let fall = GR_FALL_DB_S * (end - start) as f32 / self.sr as f32;
        self.gr_disp = gr_max.max(self.gr_disp - fall).max(0.0);
    }
}

impl Processor for Compressor {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        let max = ((MAX_LOOKAHEAD_MS * 0.001) as f64 * sample_rate).ceil() as usize + 1;
        self.delay = [DelayLine::new(max), DelayLine::new(max)];
        self.rms_coeff = one_pole_coeff(10.0, sample_rate);
        self.slow_coeff = one_pole_coeff(600.0, sample_rate);
        self.makeup.set_time(20.0, sample_rate);
        self.mix.set_time(20.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for d in &mut self.delay {
            d.clear();
        }
        for d in &mut self.det {
            d.reset();
        }
        for s in self.hpf_st.iter_mut().chain(&mut self.bell_st).chain(&mut self.lpf_st) {
            s.reset();
        }
        self.ms = [0.0; 2];
        self.vca = [0.0; 2];
        self.fb_y = [0.0; 2];
        self.gr_slow = 0.0;
        self.gr_now = 0.0;
        self.gr_disp = 0.0;
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
        let la = ((self.params.plain(p::LOOKAHEAD) * 0.001) as f64 * self.sr).round() as u32;
        la.min(self.delay[0].max_delay() as u32)
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
        write_meter(out, o, &[self.latency() as f32, self.style as f32, if self.feedback { 1.0 } else { 0.0 }])
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

    #[test]
    fn static_curve_hard_knee() {
        for &(in_db, thr, ratio) in &[(-8.0f32, -20.0f32, 4.0f32), (-3.0, -24.0, 2.0), (-30.0, -20.0, 4.0), (-6.0, -30.0, 100.0)] {
            let mut c = Compressor::new();
            set_plain(&mut c, p::THRESHOLD, thr);
            set_plain(&mut c, p::RATIO, ratio);
            set_plain(&mut c, p::KNEE, 0.0);
            set_plain(&mut c, p::ATTACK, 1.0);
            set_plain(&mut c, p::RELEASE, 200.0);
            let amp = db_to_gain(in_db);
            let (l, _) = process_stereo(&mut c, sine(1000.0, amp), 48_000);
            let out = level_db(&l[36_000..]);
            let over = (in_db - thr).max(0.0);
            let want = in_db - over * (1.0 - 1.0 / ratio);
            assert!((out - want).abs() < 0.5, "in {in_db} thr {thr} ratio {ratio}: out {out}, want {want}");
            assert!((c.static_curve(in_db) - want).abs() < 1e-3);
        }
    }

    #[test]
    fn attack_and_release_timing() {
        let mut c = Compressor::new();
        set_plain(&mut c, p::THRESHOLD, -40.0);
        set_plain(&mut c, p::RATIO, 100.0);
        set_plain(&mut c, p::KNEE, 0.0);
        set_plain(&mut c, p::ATTACK, 10.0);
        set_plain(&mut c, p::RELEASE, 100.0);
        // DC step from silence to 0 dBFS: GR heads to 40 dB.
        let (l, _) = process_stereo(&mut c, |i| if i < 4800 { (0.0, 0.0) } else if i < 4800 + 24_000 { (1.0, 1.0) } else { (0.01, 0.01) }, 4800 + 24_000 + 48_000);
        let gr_at = |i: usize, x: f32| -gain_to_db(l[i] / x);
        // After one attack time constant ≈ 63 % of 40 dB.
        let g1 = gr_at(4800 + 480, 1.0);
        assert!(g1 > 18.0 && g1 < 32.0, "{g1}");
        assert!(gr_at(4800 + 20_000, 1.0) > 39.0);
        // 100 ms after the drop, still reducing but less.
        let g2 = gr_at(4800 + 24_000 + 4800, 0.01);
        assert!(g2 > 3.0 && g2 < 30.0, "{g2}");
    }

    #[test]
    fn mix_zero_is_dry() {
        let mut c = Compressor::new();
        set_plain(&mut c, p::THRESHOLD, -50.0);
        set_plain(&mut c, p::MIX, 0.0);
        let (l, _) = process_stereo(&mut c, sine(440.0, 0.8), 4800);
        for (i, v) in l.iter().enumerate() {
            assert!((v - sine(440.0, 0.8)(i).0).abs() < 1e-6);
        }
    }

    #[test]
    fn sidechain_keys_the_compression() {
        let mut c = Compressor::new();
        set_plain(&mut c, p::THRESHOLD, -30.0);
        set_plain(&mut c, p::RATIO, 10.0);
        set_plain(&mut c, p::SIDECHAIN, 1.0);
        c.prepare(SR, BLOCK);
        let mut buf = AudioBuffer::new(2, BLOCK);
        let mut sc = AudioBuffer::new(2, BLOCK);
        let tr = Transport::default();
        let mut run = |c: &mut Compressor, sc_level: f32| {
            let mut out = 0.0f32;
            for _ in 0..200 {
                for ch in 0..2 {
                    buf.channel_mut(ch).fill(0.1);
                    sc.channel_mut(ch).fill(sc_level);
                }
                let mut ctx = ProcessContext { sample_rate: SR, buffer: &mut buf, sidechain: Some(&sc), events: &[], transport: &tr };
                c.process(&mut ctx);
                out = buf.channel(0)[BLOCK - 1];
            }
            out
        };
        let quiet = run(&mut c, 0.0);
        assert!((quiet - 0.1).abs() < 1e-3, "{quiet}");
        let keyed = run(&mut c, 1.0);
        assert!(keyed < 0.02, "{keyed}");
    }

    #[test]
    fn lookahead_reports_latency() {
        let mut c = Compressor::new();
        set_plain(&mut c, p::LOOKAHEAD, 5.0);
        c.prepare(SR, BLOCK);
        assert_eq!(c.latency(), 240);
        let (l, _) = process_stereo(&mut c, |i| if i == 100 { (1.0, 1.0) } else { (0.0, 0.0) }, 1024);
        let (imax, _) = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap();
        assert_eq!(imax, 340);
    }

    fn steady_out_db(c: &mut Compressor, in_db: f32, freq: f32) -> f32 {
        let (l, _) = process_stereo(c, sine(freq, db_to_gain(in_db)), 48_000);
        level_db(&l[36_000..])
    }

    #[test]
    fn styles_resolve_from_style_and_model() {
        assert_eq!(resolved_style(2, 0), 2);
        assert_eq!(resolved_style(2, 1), 4);
        assert_eq!(resolved_style(0, 2), 5);
        assert_eq!(STYLE_NAMES[resolved_style(3, 0)], "Opto");
        assert_eq!(effective_knee(5, 4.0), 18.0);
        assert_eq!(effective_knee(4, 4.0), 4.0);
        let mut c = Compressor::new();
        set_plain(&mut c, p::MODEL, model::VARI_MU);
        process_stereo(&mut c, |_| (0.0, 0.0), 256);
        let mut m = [0.0f32; 80];
        assert_eq!(c.meter(&mut m), 70);
        assert_eq!(m[68], 5.0);
        // Vari-Mu's wide knee already works at the threshold.
        assert!(c.static_curve(-18.0) < -18.5);
    }

    #[test]
    fn vca_releases_at_a_constant_rate_in_db() {
        let mut c = Compressor::new();
        set_plain(&mut c, p::MODEL, model::VCA);
        set_plain(&mut c, p::THRESHOLD, -40.0);
        set_plain(&mut c, p::RATIO, 100.0);
        set_plain(&mut c, p::KNEE, 0.0);
        set_plain(&mut c, p::ATTACK, 0.5);
        set_plain(&mut c, p::RELEASE, 100.0);
        // 0 dBFS DC (≈ 40 dB of reduction), then near silence.
        let drop = 24_000;
        let (l, _) = process_stereo(&mut c, |i| if i < drop { (1.0, 1.0) } else { (1e-4, 1e-4) }, drop + 24_000);
        let gr_at = |i: usize| -gain_to_db(l[i] / 1e-4);
        assert!((-gain_to_db(l[drop - 1]) - 39.6).abs() < 0.5);
        // 20 dB per 100 ms: 10 dB after 50 ms, 20 dB after 100 ms.
        let g1 = gr_at(drop + 2_400);
        let g2 = gr_at(drop + 4_800);
        assert!((g1 - 29.6).abs() < 1.0, "{g1}");
        assert!((g2 - 19.6).abs() < 1.0, "{g2}");
    }

    #[test]
    fn feedback_lands_on_the_same_static_curve() {
        for &(in_db, thr, ratio) in &[(-6.0f32, -24.0f32, 4.0f32), (-10.0, -30.0, 2.0), (0.0, -20.0, 10.0), (-30.0, -20.0, 4.0)] {
            let mut c = Compressor::new();
            set_plain(&mut c, p::TOPOLOGY, topology::FEEDBACK);
            set_plain(&mut c, p::THRESHOLD, thr);
            set_plain(&mut c, p::RATIO, ratio);
            set_plain(&mut c, p::KNEE, 0.0);
            set_plain(&mut c, p::ATTACK, 2.0);
            set_plain(&mut c, p::RELEASE, 300.0);
            let out = steady_out_db(&mut c, in_db, 1000.0);
            let want = c.static_curve(in_db);
            assert!((out - want).abs() < 0.8, "in {in_db} thr {thr} ratio {ratio}: out {out}, want {want}");
            let mut m = [0.0f32; 80];
            c.meter(&mut m);
            assert_eq!(m[69], 1.0);
        }
    }

    #[test]
    fn feedback_is_stable_at_extreme_settings() {
        let mut c = Compressor::new();
        set_plain(&mut c, p::TOPOLOGY, topology::FEEDBACK);
        set_plain(&mut c, p::THRESHOLD, -60.0);
        set_plain(&mut c, p::RATIO, 100.0);
        set_plain(&mut c, p::ATTACK, 0.01);
        set_plain(&mut c, p::RELEASE, 5.0);
        set_plain(&mut c, p::KNEE, 0.0);
        let mut seed = 7u32;
        let (l, _) = process_stereo(&mut c, |_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
            (v * 1.8, v * 1.8)
        }, 48_000);
        assert!(l.iter().all(|x| x.is_finite() && x.abs() <= 0.91), "the loop never gains");
        // Deep, steady reduction rather than oscillation.
        let tail = rms(&l[24_000..]);
        assert!(tail < 0.1, "{tail}");
    }

    #[test]
    fn sidechain_eq_shapes_what_the_detector_hears() {
        let gr_with = |set: &dyn Fn(&mut Compressor), freq: f32| {
            let mut c = Compressor::new();
            set_plain(&mut c, p::THRESHOLD, -20.0);
            set_plain(&mut c, p::RATIO, 4.0);
            set_plain(&mut c, p::KNEE, 0.0);
            set(&mut c);
            -steady_out_db(&mut c, -12.0, freq) - 12.0
        };
        let plain = gr_with(&|_| {}, 100.0);
        assert!((plain - 6.0).abs() < 0.5, "{plain}");
        // A +12 dB bell at 100 Hz in the detector: 12 dB more over.
        let bell = gr_with(&|c| {
            set_plain(c, p::SC_EQ_FREQ, 100.0);
            set_plain(c, p::SC_EQ_GAIN, 12.0);
        }, 100.0);
        assert!((bell - 15.0).abs() < 1.0, "{bell}");
        // Low-pass at 1 kHz: a 6 kHz tone barely reaches the detector.
        let lp = gr_with(&|c| set_plain(c, p::SC_LPF, 1000.0), 6000.0);
        assert!(lp < 0.5, "{lp}");
        assert!((gr_with(&|_| {}, 6000.0) - 6.0).abs() < 0.5);
    }

    #[test]
    fn no_nan_or_denormals_in_any_style() {
        for (st, md, topo) in [(0.0, 0.0, 0.0), (1.0, 0.0, 1.0), (2.0, 0.0, 0.0), (3.0, 0.0, 1.0), (0.0, 1.0, 0.0), (0.0, 1.0, 1.0), (0.0, 2.0, 1.0)] {
            let mut c = Compressor::new();
            set_plain(&mut c, p::STYLE, st);
            set_plain(&mut c, p::MODEL, md);
            set_plain(&mut c, p::TOPOLOGY, topo);
            set_plain(&mut c, p::THRESHOLD, -40.0);
            set_plain(&mut c, p::SC_EQ_GAIN, 18.0);
            set_plain(&mut c, p::SC_LPF, 2000.0);
            set_plain(&mut c, p::SC_HPF, 200.0);
            set_plain(&mut c, p::LOOKAHEAD, 3.0);
            let (l, r) = process_stereo(&mut c, |i| if i < 9600 { sine(80.0, 0.9)(i) } else { (0.0, 0.0) }, 96_000);
            for x in l.iter().chain(&r) {
                assert!(x.is_finite() && (*x == 0.0 || x.abs() > 1e-30), "style {st}/{md}/{topo}: {x:e}");
            }
        }
    }

    /// Release builds only (`cargo test -p aitrack-stock --release compressor::tests::cpu`).
    #[test]
    #[cfg_attr(debug_assertions, ignore)]
    fn cpu_budget_per_instance() {
        let mut c = Compressor::new();
        set_plain(&mut c, p::MODEL, model::VARI_MU);
        set_plain(&mut c, p::TOPOLOGY, topology::FEEDBACK);
        set_plain(&mut c, p::SC_HPF, 100.0);
        set_plain(&mut c, p::SC_EQ_GAIN, 6.0);
        set_plain(&mut c, p::SC_LPF, 8000.0);
        set_plain(&mut c, p::LOOKAHEAD, 5.0);
        set_plain(&mut c, p::THRESHOLD, -30.0);
        let seconds = 2.0;
        let t = std::time::Instant::now();
        process_stereo(&mut c, sine(110.0, 0.7), (seconds * SR) as usize);
        let share = t.elapsed().as_secs_f64() / seconds;
        assert!(share < 0.01, "{:.3} % of a core", share * 100.0);
    }
}
