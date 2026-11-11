//! `fx.compressor` — feed-forward compressor after Giannoulis, Massberg &
//! Reiss (JAES 2012): log-domain gain computer with a quadratic soft knee
//! and a smooth decoupled peak detector.
//!
//! Styles (topology presets, not emulations):
//! * **Clean** — smooth decoupled detector, times as set.
//! * **Punch** — branching detector, attack ×1.6 and release ×0.6 so
//!   transients poke through and the body pumps.
//! * **Glue** — at least 60 % RMS detection and program-dependent release
//!   (fast for transients, slow for sustained reduction).
//! * **Opto** — attack ×2, knee ≥ 6 dB and a release that slows as the
//!   reduction history grows (two-stage memory).
//!
//! Lookahead delays the audio by the lookahead time; `latency()` reports the
//! current lookahead in samples (changing it changes the reported latency,
//! so hosts re-run delay compensation). Sidechain: internal or the external
//! sidechain bus, with a 12 dB/oct high-pass (off at 20 Hz).
//!
//! Meter layout (67 values): `[0]` gain reduction in dB (≤ 0), `[1]` input
//! peak dBFS, `[2]` output peak dBFS, `[3..67)` static curve: output level
//! in dB for inputs from −60 to 0 dB in 64 steps (makeup included).

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
}

/// Style indices.
pub mod style {
    pub const CLEAN: f32 = 0.0;
    pub const PUNCH: f32 = 1.0;
    pub const GLUE: f32 = 2.0;
    pub const OPTO: f32 = 3.0;
}

const OFF_ON: &[&str] = &["Off", "On"];
const STYLES: &[&str] = &["Clean", "Punch", "Glue", "Opto"];
const SC: &[&str] = &["Internal", "External"];
const MAX_LOOKAHEAD_MS: f32 = 10.0;
const CONTROL: usize = 16;

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
    ]
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
    listen: bool,
    hpf_on: bool,
    hpf: SvfCoeffs,
    hpf_st: [SvfState; 2],
    lookahead: usize,
    delay: [DelayLine; 2],
    rms_coeff: f32,
    ms: [f32; 2],
    det: [GainSmoother; 2],
    gr_slow: f32,
    slow_coeff: f32,
    makeup: Smoother,
    mix: Smoother,
    gr_now: f32,
    in_peak: f32,
    out_peak: f32,
    first: bool,
    counter: usize,
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
            listen: false,
            hpf_on: false,
            hpf: SvfCoeffs::default(),
            hpf_st: [SvfState::default(); 2],
            lookahead: 0,
            delay: [DelayLine::new(1), DelayLine::new(1)],
            rms_coeff: 0.01,
            ms: [0.0; 2],
            det: [GainSmoother::default(); 2],
            gr_slow: 0.0,
            slow_coeff: 0.0,
            makeup: Smoother::new(1.0),
            mix: Smoother::new(1.0),
            gr_now: 0.0,
            in_peak: 0.0,
            out_peak: 0.0,
            first: true,
            counter: 0,
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
        if self.style == 3 { self.knee.max(6.0) } else { self.knee }
    }

    fn update(&mut self) {
        let ps = &self.params;
        self.thr = ps.plain(p::THRESHOLD);
        self.ratio = ps.plain(p::RATIO);
        self.knee = ps.plain(p::KNEE);
        self.range = ps.plain(p::RANGE);
        self.attack_ms = ps.plain(p::ATTACK);
        self.release_ms = ps.plain(p::RELEASE);
        self.style = ps.choice(p::STYLE).min(3);
        self.auto_release = ps.on(p::AUTO_RELEASE) || self.style == 2;
        self.rms_mix = ps.plain(p::RMS) / 100.0;
        if self.style == 2 {
            self.rms_mix = self.rms_mix.max(0.6);
        }
        self.link = ps.plain(p::LINK) / 100.0;
        self.external = ps.choice(p::SIDECHAIN) == 1;
        self.listen = ps.on(p::SC_LISTEN);
        let hpf = ps.plain(p::SC_HPF);
        self.hpf_on = hpf > 20.5;
        self.hpf = SvfCoeffs::new(SvfMode::Highpass, hpf, std::f32::consts::FRAC_1_SQRT_2, 0.0, self.sr);
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
        let (a, r) = match self.style {
            1 => (self.attack_ms * 1.6, release_ms * 0.6),
            3 => (self.attack_ms * 2.0, release_ms),
            _ => (self.attack_ms, release_ms),
        };
        for d in &mut self.det {
            d.set_times(a, r, self.sr);
        }
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
        let (thr, ratio, knee, range) = (self.thr, self.ratio, self.effective_knee(), self.range);
        let branching = self.style == 1;
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
                let (dl, dr) = if external { sidechain_frame(sc, n) } else { (l, r) };
                let (mut dl, mut dr) = (dl, dr);
                if this.hpf_on {
                    dl = this.hpf_st[0].process(&this.hpf, dl);
                    dr = this.hpf_st[1].process(&this.hpf, dr);
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
                    let target = gain_computer(d, thr, ratio, knee).min(range);
                    gr[c] = if branching { this.det[c].process_branching(target) } else { this.det[c].process(target) };
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
                if this.listen {
                    return (sanitize(dl), sanitize(dr));
                }
                let mk = this.makeup.next();
                let mix = this.mix.next();
                let wl = xl * db_to_gain(-gr[0]) * mk;
                let wr = xr * db_to_gain(-gr[1]) * mk;
                let ol = xl + (wl - xl) * mix;
                let or = xr + (wr - xr) * mix;
                out_peak = out_peak.max(ol.abs()).max(or.abs());
                (sanitize(ol), sanitize(or))
            });
            i = e;
        }
        self.in_peak = in_peak;
        self.out_peak = out_peak;
        self.counter = self.counter.wrapping_add(1);
        let _ = gr_max;
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
        for s in &mut self.hpf_st {
            s.reset();
        }
        self.ms = [0.0; 2];
        self.gr_slow = 0.0;
        self.gr_now = 0.0;
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
        let mut o = write_meter(out, 0, &[-self.gr_now, gain_to_db(self.in_peak), gain_to_db(self.out_peak)]);
        let mut curve = [0.0f32; 64];
        for (i, c) in curve.iter_mut().enumerate() {
            *c = self.static_curve(-60.0 + 60.0 * i as f32 / 63.0);
        }
        o = write_meter(out, o, &curve);
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{BLOCK, SR, peak, process_stereo, set_plain, sine};
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
}
