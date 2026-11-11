//! `fx.gate` — gate / expander / ducker with hysteresis, hold and lookahead.
//!
//! * **Gate**: opens when the detector rises above the threshold and closes
//!   once it falls below `threshold − hysteresis` and the hold time has run
//!   out; closed means the range attenuation.
//! * **Expander**: below the threshold the level is pushed down by
//!   `(threshold − level) · (ratio − 1)` dB, down to the range.
//! * **Duck**: the inverse gate — the signal is attenuated by the range while
//!   the (usually external) sidechain is above the threshold.
//!
//! The detector is a fast peak follower on the (optionally external)
//! sidechain, filtered by a high-pass and a low-pass. Gain moves in dB with
//! the attack time on trigger (opening, or ducking down in duck mode) and
//! the release time when the trigger ends. Lookahead delays the
//! audio; `latency()` reports it.
//!
//! Meter layout: `[0]` gate gain dB (≤ 0), `[1]` detector level dB,
//! `[2]` open flag (1 open, 0 closed).

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, sidechain_frame, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use aitrack_dsp::smooth::one_pole_coeff;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.gate";

pub mod p {
    pub const THRESHOLD: u32 = 0;
    pub const HYSTERESIS: u32 = 1;
    pub const ATTACK: u32 = 2;
    pub const HOLD: u32 = 3;
    pub const RELEASE: u32 = 4;
    pub const RANGE: u32 = 5;
    pub const MODE: u32 = 6;
    pub const RATIO: u32 = 7;
    pub const LOOKAHEAD: u32 = 8;
    pub const SIDECHAIN: u32 = 9;
    pub const SC_HPF: u32 = 10;
    pub const SC_LPF: u32 = 11;
}

/// Mode indices.
pub mod mode {
    pub const GATE: f32 = 0.0;
    pub const EXPANDER: f32 = 1.0;
    pub const DUCK: f32 = 2.0;
}

const MODES: &[&str] = &["Gate", "Expander", "Duck"];
const SC: &[&str] = &["Internal", "External"];
const MAX_LOOKAHEAD_MS: f32 = 10.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::THRESHOLD, "Threshold", -80.0, 0.0, -40.0).unit("dB").short("Thresh"),
        ParamInfo::new(p::HYSTERESIS, "Hysteresis", 0.0, 20.0, 4.0).unit("dB").short("Hyst"),
        ParamInfo::new(p::ATTACK, "Attack", 0.01, 50.0, 0.5).unit("ms").log().short("Att"),
        ParamInfo::new(p::HOLD, "Hold", 0.0, 500.0, 20.0).unit("ms").power(2.0),
        ParamInfo::new(p::RELEASE, "Release", 1.0, 2_000.0, 100.0).unit("ms").log().short("Rel"),
        ParamInfo::new(p::RANGE, "Range", -80.0, 0.0, -80.0).unit("dB"),
        ParamInfo::new(p::MODE, "Mode", 0.0, 2.0, mode::GATE).choices(MODES),
        ParamInfo::new(p::RATIO, "Ratio", 1.0, 10.0, 4.0).log(),
        ParamInfo::new(p::LOOKAHEAD, "Lookahead", 0.0, MAX_LOOKAHEAD_MS, 0.0).unit("ms").short("Look"),
        ParamInfo::new(p::SIDECHAIN, "Sidechain", 0.0, 1.0, 0.0).choices(SC).short("SC"),
        ParamInfo::new(p::SC_HPF, "SC High-Pass", 20.0, 2_000.0, 20.0).unit("Hz").log().short("HPF"),
        ParamInfo::new(p::SC_LPF, "SC Low-Pass", 200.0, 20_000.0, 20_000.0).unit("Hz").log().short("LPF"),
    ]
}

pub struct Gate {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    thr: f32,
    close_thr: f32,
    range: f32,
    ratio: f32,
    mode: usize,
    external: bool,
    hold_samples: u32,
    hold_left: u32,
    open: bool,
    att: f32,
    rel: f32,
    gain_db: f32,
    env: EnvelopeFollower,
    hpf_on: bool,
    lpf_on: bool,
    hpf: SvfCoeffs,
    lpf: SvfCoeffs,
    filt: [[SvfState; 2]; 2],
    lookahead: usize,
    delay: [DelayLine; 2],
    meter_det: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Gate::new())
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub fn new() -> Self {
        let params = params();
        let mut g = Self {
            info: PluginInfo {
                id: ID,
                name: "Gate",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "gate", "expander", "duck"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: true,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            thr: -40.0,
            close_thr: -44.0,
            range: -80.0,
            ratio: 4.0,
            mode: 0,
            external: false,
            hold_samples: 0,
            hold_left: 0,
            open: false,
            att: 1.0,
            rel: 1.0,
            gain_db: -80.0,
            env: EnvelopeFollower::new(DetectorMode::Peak),
            hpf_on: false,
            lpf_on: false,
            hpf: SvfCoeffs::default(),
            lpf: SvfCoeffs::default(),
            filt: [[SvfState::default(); 2]; 2],
            lookahead: 0,
            delay: [DelayLine::new(1), DelayLine::new(1)],
            meter_det: -120.0,
        };
        g.prepare(48_000.0, 512);
        g
    }

    fn update(&mut self) {
        let ps = &self.params;
        self.thr = ps.plain(p::THRESHOLD);
        self.close_thr = self.thr - ps.plain(p::HYSTERESIS);
        self.range = ps.plain(p::RANGE);
        self.ratio = ps.plain(p::RATIO);
        self.mode = ps.choice(p::MODE).min(2);
        self.external = ps.choice(p::SIDECHAIN) == 1;
        self.hold_samples = ((ps.plain(p::HOLD) * 0.001) as f64 * self.sr) as u32;
        self.att = one_pole_coeff(ps.plain(p::ATTACK), self.sr);
        self.rel = one_pole_coeff(ps.plain(p::RELEASE), self.sr);
        let hpf = ps.plain(p::SC_HPF);
        let lpf = ps.plain(p::SC_LPF);
        self.hpf_on = hpf > 20.5;
        self.lpf_on = lpf < 19_900.0;
        self.hpf = SvfCoeffs::new(SvfMode::Highpass, hpf, std::f32::consts::FRAC_1_SQRT_2, 0.0, self.sr);
        self.lpf = SvfCoeffs::new(SvfMode::Lowpass, lpf, std::f32::consts::FRAC_1_SQRT_2, 0.0, self.sr);
        let la = ((ps.plain(p::LOOKAHEAD) * 0.001) as f64 * self.sr).round() as usize;
        self.lookahead = la.min(self.delay[0].max_delay());
    }

    /// Target gain (dB) for a detector level (dB), updating the gate state.
    #[inline]
    fn target(&mut self, lvl: f32) -> f32 {
        match self.mode {
            1 => (-(self.thr - lvl).max(0.0) * (self.ratio - 1.0)).max(self.range),
            _ => {
                if lvl >= self.thr {
                    self.open = true;
                    self.hold_left = self.hold_samples;
                } else if lvl < self.close_thr {
                    if self.hold_left > 0 {
                        self.hold_left -= 1;
                    } else {
                        self.open = false;
                    }
                }
                let pass = if self.mode == 2 { !self.open } else { self.open };
                if pass { 0.0 } else { self.range }
            }
        }
    }
}

impl Segmented for Gate {
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
        let la = self.lookahead;
        let mut det_max = -120.0f32;
        let this = &mut *self;
        for_frames(ctx.buffer, start, end, |n, l, r| {
            let (mut dl, mut dr) = if external { sidechain_frame(sc, n) } else { (l, r) };
            if this.hpf_on {
                dl = this.filt[0][0].process(&this.hpf, dl);
                dr = this.filt[1][0].process(&this.hpf, dr);
            }
            if this.lpf_on {
                dl = this.filt[0][1].process(&this.lpf, dl);
                dr = this.filt[1][1].process(&this.lpf, dr);
            }
            let lvl = gain_to_db(this.env.process(dl.abs().max(dr.abs())));
            det_max = det_max.max(lvl);
            let t = this.target(lvl);
            // Attack is the trigger response: opening for gate/expander,
            // ducking down for duck.
            let rising = t > this.gain_db;
            let c = if rising != (this.mode == 2) { this.att } else { this.rel };
            this.gain_db += (t - this.gain_db) * c;
            let g = db_to_gain(this.gain_db);
            this.delay[0].push(l);
            this.delay[1].push(r);
            (sanitize(this.delay[0].read(la) * g), sanitize(this.delay[1].read(la) * g))
        });
        self.meter_det = det_max;
    }
}

impl Processor for Gate {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        let max = ((MAX_LOOKAHEAD_MS * 0.001) as f64 * sample_rate).ceil() as usize + 1;
        self.delay = [DelayLine::new(max), DelayLine::new(max)];
        // Fast peak detector: instant-ish attack, 10 ms fall.
        self.env.set_times(0.05, 10.0, sample_rate);
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for d in &mut self.delay {
            d.clear();
        }
        self.filt = [[SvfState::default(); 2]; 2];
        self.env.reset();
        self.open = false;
        self.hold_left = 0;
        self.gain_db = if self.mode == 2 { 0.0 } else { self.range };
        self.meter_det = -120.0;
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
        write_meter(out, 0, &[self.gain_db.min(0.0), self.meter_det, if self.open { 1.0 } else { 0.0 }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{peak, process_stereo, set_plain};

    fn burst(i: usize) -> (f32, f32) {
        // 100 ms of −6 dB tone, 100 ms of −60 dB tone, repeating.
        let amp = if (i / 4800) % 2 == 0 { 0.5 } else { 0.001 };
        let v = amp * (std::f32::consts::TAU * 200.0 * i as f32 / 48_000.0).sin();
        (v, v)
    }

    #[test]
    fn gate_opens_and_closes() {
        let mut g = Gate::new();
        set_plain(&mut g, p::THRESHOLD, -30.0);
        set_plain(&mut g, p::RANGE, -80.0);
        set_plain(&mut g, p::HOLD, 5.0);
        set_plain(&mut g, p::RELEASE, 5.0);
        let (l, _) = process_stereo(&mut g, burst, 19_200);
        assert!(peak(&l[1000..4800]) > 0.45, "open");
        assert!(peak(&l[7000..9600]) < 1e-6, "closed: {}", peak(&l[7000..9600]));
        assert!(peak(&l[10_600..14_400]) > 0.45, "reopens");
    }

    #[test]
    fn hysteresis_keeps_it_open() {
        let mut g = Gate::new();
        set_plain(&mut g, p::THRESHOLD, -20.0);
        set_plain(&mut g, p::HYSTERESIS, 12.0);
        set_plain(&mut g, p::HOLD, 0.0);
        // Starts at −6 dB, then sits at −26 dB (between close and open).
        let (l, _) = process_stereo(&mut g, |i| {
            let amp = if i < 4800 { 0.5 } else { 0.05 };
            let v = amp * (std::f32::consts::TAU * 200.0 * i as f32 / 48_000.0).sin();
            (v, v)
        }, 14_400);
        assert!(peak(&l[9600..]) > 0.045);
    }

    #[test]
    fn expander_and_duck() {
        let mut g = Gate::new();
        set_plain(&mut g, p::MODE, mode::EXPANDER);
        set_plain(&mut g, p::THRESHOLD, -20.0);
        set_plain(&mut g, p::RATIO, 2.0);
        set_plain(&mut g, p::RANGE, -60.0);
        let (l, _) = process_stereo(&mut g, |i| {
            let v = 0.01 * (std::f32::consts::TAU * 200.0 * i as f32 / 48_000.0).sin();
            (v, v)
        }, 24_000);
        // −40 dB in, 20 dB under the threshold, ratio 2 → −20 dB more.
        let out = gain_to_db(peak(&l[12_000..]));
        assert!((out + 60.0).abs() < 1.5, "{out}");

        let mut d = Gate::new();
        set_plain(&mut d, p::MODE, mode::DUCK);
        set_plain(&mut d, p::THRESHOLD, -30.0);
        set_plain(&mut d, p::RANGE, -12.0);
        let (l, _) = process_stereo(&mut d, burst, 9600);
        let ducked = gain_to_db(peak(&l[2400..4800]));
        assert!((ducked - (-6.0 - 12.0)).abs() < 0.5, "{ducked}");
    }

    #[test]
    fn lookahead_latency() {
        let mut g = Gate::new();
        set_plain(&mut g, p::LOOKAHEAD, 2.0);
        assert_eq!(g.latency(), 96);
    }
}
