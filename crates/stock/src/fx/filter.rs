//! `fx.filter` — multimode TPT state-variable filter (12/24 dB) with drive,
//! an LFO (free or tempo-synced) and an envelope follower modulating the
//! cutoff in octaves. Coefficients update every 8 samples from a smoothed
//! log-frequency, so sweeps are zipper-free. Zero latency.
//!
//! Meter layout: `[cutoff L (Hz), cutoff R (Hz), envelope (0..1)]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use aitrack_dsp::lfo::{Lfo, LfoShape, SYNC_DIVISIONS, division_beats, division_hz};
use aitrack_dsp::shaper::{Adaa1, ShaperKind};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState, butterworth_q};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.filter";

pub mod p {
    pub const TYPE: u32 = 0;
    pub const SLOPE: u32 = 1;
    pub const CUTOFF: u32 = 2;
    pub const RESONANCE: u32 = 3;
    pub const DRIVE: u32 = 4;
    pub const LFO_SHAPE: u32 = 5;
    pub const LFO_SYNC: u32 = 6;
    pub const LFO_RATE: u32 = 7;
    pub const LFO_DIV: u32 = 8;
    pub const LFO_DEPTH: u32 = 9;
    pub const LFO_STEREO: u32 = 10;
    pub const ENV_AMOUNT: u32 = 11;
    pub const ENV_ATTACK: u32 = 12;
    pub const ENV_RELEASE: u32 = 13;
    pub const MIX: u32 = 14;
    pub const OUTPUT: u32 = 15;
}

const TYPES: &[&str] = &["Low-pass", "High-pass", "Band-pass", "Notch", "Peak"];
const SLOPES: &[&str] = &["12 dB/oct", "24 dB/oct"];
const OFF_ON: &[&str] = &["Off", "On"];
const UPDATE: usize = 8;

pub fn params() -> Vec<ParamInfo> {
    let n_div = (SYNC_DIVISIONS.len() - 1) as f32;
    vec![
        ParamInfo::new(p::TYPE, "Type", 0.0, 4.0, 0.0).choices(TYPES),
        ParamInfo::new(p::SLOPE, "Slope", 0.0, 1.0, 0.0).choices(SLOPES),
        ParamInfo::new(p::CUTOFF, "Cutoff", 20.0, 20_000.0, 1000.0).unit("Hz").log(),
        ParamInfo::new(p::RESONANCE, "Resonance", 0.0, 100.0, 15.0).unit("%").short("Reso"),
        ParamInfo::new(p::DRIVE, "Drive", 0.0, 24.0, 0.0).unit("dB"),
        ParamInfo::new(p::LFO_SHAPE, "LFO Shape", 0.0, (LfoShape::NAMES.len() - 1) as f32, 0.0).choices(LfoShape::NAMES).short("Shape"),
        ParamInfo::new(p::LFO_SYNC, "LFO Sync", 0.0, 1.0, 0.0).choices(OFF_ON).short("Sync"),
        ParamInfo::new(p::LFO_RATE, "LFO Rate", 0.01, 20.0, 1.0).unit("Hz").log().short("Rate"),
        ParamInfo::new(p::LFO_DIV, "LFO Division", 0.0, n_div, 7.0).choices(SYNC_DIVISIONS).short("Div"),
        ParamInfo::new(p::LFO_DEPTH, "LFO Depth", -4.0, 4.0, 0.0).unit("oct").short("Depth"),
        ParamInfo::new(p::LFO_STEREO, "LFO Stereo Phase", 0.0, 180.0, 0.0).unit("°").short("Stereo"),
        ParamInfo::new(p::ENV_AMOUNT, "Envelope Amount", -4.0, 4.0, 0.0).unit("oct").short("Env"),
        ParamInfo::new(p::ENV_ATTACK, "Envelope Attack", 0.1, 100.0, 5.0).unit("ms").log().short("Att"),
        ParamInfo::new(p::ENV_RELEASE, "Envelope Release", 5.0, 2000.0, 150.0).unit("ms").log().short("Rel"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
    ]
}

/// Resonance 0..100 % → Q 0.5..25 (exponential).
pub fn resonance_q(res: f32) -> f32 {
    0.5 * 50f32.powf((res / 100.0).clamp(0.0, 1.0))
}

#[derive(Clone, Copy, Default)]
struct Channel {
    st: [SvfState; 2],
    c: [SvfCoeffs; 2],
    shaper: Option<Adaa1>,
    cutoff: f32,
}

pub struct Filter {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    ch: [Channel; 2],
    lfo: [Lfo; 2],
    env: EnvelopeFollower,
    base_oct: Smoother,
    wet: Smoother,
    dry: Smoother,
    drive: Smoother,
    counter: usize,
    env_level: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Filter::new())
}

impl Default for Filter {
    fn default() -> Self {
        Self::new()
    }
}

impl Filter {
    pub fn new() -> Self {
        let params = params();
        let mut f = Self {
            info: PluginInfo {
                id: ID,
                name: "Filter",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["filter", "modulation", "lfo", "envelope"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            ch: [Channel { shaper: Some(Adaa1::new(ShaperKind::Tanh)), cutoff: 1000.0, ..Default::default() }; 2],
            lfo: [Lfo::new(11), Lfo::new(12)],
            env: EnvelopeFollower::new(DetectorMode::Peak),
            base_oct: Smoother::new(0.0),
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            drive: Smoother::new(1.0),
            counter: 0,
            env_level: 0.0,
            first: true,
        };
        f.prepare(48_000.0, 512);
        f
    }

    fn update(&mut self) {
        // Cutoff smoothed in octaves above 20 Hz.
        self.base_oct.set_target((self.params.plain(p::CUTOFF) / 20.0).log2());
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        self.drive.set_target(db_to_gain(self.params.plain(p::DRIVE)));
        let shape = LfoShape::from_index(self.params.choice(p::LFO_SHAPE));
        for l in &mut self.lfo {
            l.shape = shape;
        }
        self.env.set_times(self.params.plain(p::ENV_ATTACK), self.params.plain(p::ENV_RELEASE), self.sr);
        if self.first {
            for s in [&mut self.base_oct, &mut self.wet, &mut self.dry, &mut self.drive] {
                s.snap_to_target();
            }
            self.first = false;
        }
    }

    fn design(&mut self, ch: usize, cutoff: f32) {
        let mode = match self.params.choice(p::TYPE) {
            1 => SvfMode::Highpass,
            2 => SvfMode::Bandpass,
            3 => SvfMode::Notch,
            4 => SvfMode::Peak,
            _ => SvfMode::Lowpass,
        };
        let q = resonance_q(self.params.plain(p::RESONANCE));
        let c = &mut self.ch[ch];
        c.cutoff = cutoff;
        if self.params.choice(p::SLOPE) == 1 {
            // Butterworth pair; resonance lifts the second section.
            let (q1, q2) = match mode {
                SvfMode::Lowpass | SvfMode::Highpass => (butterworth_q(4, 0), butterworth_q(4, 1) * q / std::f32::consts::FRAC_1_SQRT_2),
                _ => (q, q),
            };
            c.c[0] = SvfCoeffs::new(mode, cutoff, q1, 0.0, self.sr);
            c.c[1] = SvfCoeffs::new(mode, cutoff, q2, 0.0, self.sr);
        } else {
            c.c[0] = SvfCoeffs::new(mode, cutoff, q, 0.0, self.sr);
            c.c[1] = SvfCoeffs::passthrough();
        }
    }
}

impl Segmented for Filter {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        let dirty = self.params.take_dirty();
        if dirty {
            self.update();
        }
        let tr = ctx.transport;
        let sync = self.params.on(p::LFO_SYNC);
        let div = self.params.choice(p::LFO_DIV);
        let rate = if sync { division_hz(div, tr.bpm) as f32 } else { self.params.plain(p::LFO_RATE) };
        let offset = self.params.plain(p::LFO_STEREO) as f64 / 360.0;
        for l in &mut self.lfo {
            l.set_rate(rate, self.sr);
        }
        if sync && tr.playing {
            let beats = tr.position_beats + start as f64 / self.sr * tr.bpm / 60.0;
            let cycle = division_beats(div);
            self.lfo[0].sync_to_beats(beats, cycle, 0.0);
            self.lfo[1].sync_to_beats(beats, cycle, offset);
        } else if dirty {
            let ph = self.lfo[0].phase();
            self.lfo[1].set_phase(ph + offset);
        }
        let lfo_depth = self.params.plain(p::LFO_DEPTH);
        let env_amt = self.params.plain(p::ENV_AMOUNT);
        let two = self.params.choice(p::SLOPE) == 1;
        let nyq_oct = ((self.sr as f32 * 0.45) / 20.0).log2();
        if dirty {
            // Make the next sample recompute coefficients.
            self.counter = 0;
        }
        let buf = &mut *ctx.buffer;
        let mut i = start;
        while i < end {
            if self.counter == 0 {
                let base = self.base_oct.advance(UPDATE);
                let env = self.env_level.min(1.0);
                for c in 0..2 {
                    let lfo = self.lfo[c].value();
                    let oct = (base + lfo * lfo_depth + env * env_amt).clamp(0.0, nyq_oct);
                    self.design(c, 20.0 * 2f32.powf(oct));
                }
                self.counter = UPDATE;
            }
            let n = self.counter.min(end - i);
            let seg_end = i + n;
            let Self { ch, lfo, env, wet, dry, drive: drv, env_level, .. } = self;
            for_frames(buf, i, seg_end, |_, l, r| {
                *env_level = env.process(0.5 * (l + r));
                lfo[0].advance(1);
                lfo[1].advance(1);
                let g = drv.next();
                let (w, d) = (wet.next(), dry.next());
                let mut out = [l, r];
                for (k, x) in out.iter_mut().enumerate() {
                    let c = &mut ch[k];
                    let mut y = *x;
                    if g > 1.0001
                        && let Some(s) = c.shaper.as_mut()
                    {
                        y = s.process(y * g) / g.sqrt();
                    }
                    y = c.st[0].process(&c.c[0], y);
                    if two {
                        y = c.st[1].process(&c.c[1], y);
                    }
                    *x = sanitize(*x * d + y * w);
                }
                (out[0], out[1])
            });
            self.counter -= n;
            i = seg_end;
        }
    }
}

impl Processor for Filter {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.base_oct.set_time(15.0, sample_rate);
        for s in [&mut self.wet, &mut self.dry, &mut self.drive] {
            s.set_time(15.0, sample_rate);
        }
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        for c in &mut self.ch {
            for s in &mut c.st {
                s.reset();
            }
            if let Some(s) = c.shaper.as_mut() {
                s.reset();
            }
        }
        self.env.reset();
        self.env_level = 0.0;
        self.lfo[0].reset(0.0);
        self.lfo[1].reset(0.0);
        self.counter = 0;
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

    fn tail(&self) -> u32 {
        // Resonant ringing: generous fixed tail.
        (self.sr * 0.25) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.ch[0].cutoff, self.ch[1].cutoff, self.env_level.min(1.0)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, process_stereo, process_stereo_prepared, rms, set_plain, sine};
    use aitrack_dsp::Transport;

    fn gain_db(f: &mut Filter, hz: f32) -> f32 {
        let (l, _) = process_stereo(f, sine(hz, 0.1), 24_000);
        20.0 * (rms(&l[12_000..]) / (0.1 / std::f32::consts::SQRT_2)).log10()
    }

    #[test]
    fn lowpass_slopes() {
        let mut f = Filter::new();
        set_plain(&mut f, p::CUTOFF, 500.0);
        set_plain(&mut f, p::RESONANCE, 0.0); // Q 0.5
        let pass = gain_db(&mut f, 50.0);
        assert!(pass.abs() < 0.5, "{pass}");
        let a12 = gain_db(&mut f, 4000.0);
        // 3 octaves above: ≈ −36 dB for 12 dB/oct.
        assert!(a12 < -32.0 && a12 > -42.0, "{a12}");
        set_plain(&mut f, p::SLOPE, 1.0);
        let a24 = gain_db(&mut f, 4000.0);
        assert!(a24 < -66.0 && a24 > -80.0, "{a24}");
    }

    #[test]
    fn lfo_sweeps_the_cutoff() {
        let mut f = Filter::new();
        set_plain(&mut f, p::LFO_DEPTH, 2.0);
        set_plain(&mut f, p::LFO_RATE, 2.0);
        let mut seen = (f32::MAX, 0.0f32);
        f.prepare(SR, 128);
        for _ in 0..40 {
            process_stereo_prepared(&mut f, sine(100.0, 0.1), 1200, &Transport::default());
            let mut m = [0.0; 3];
            f.meter(&mut m);
            seen = (seen.0.min(m[0]), seen.1.max(m[0]));
        }
        assert!(seen.0 < 300.0 && seen.1 > 3000.0, "{seen:?}");
    }

    #[test]
    fn synced_lfo_follows_transport() {
        let mut f = Filter::new();
        set_plain(&mut f, p::LFO_SYNC, 1.0);
        set_plain(&mut f, p::LFO_DIV, 3.0); // 1 bar
        set_plain(&mut f, p::LFO_DEPTH, 1.0);
        set_plain(&mut f, p::LFO_SHAPE, 2.0); // saw up
        f.prepare(SR, 128);
        // At beat 2 of a 4-beat cycle the saw is at its midpoint (0) → base cutoff.
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 2.0, ..Transport::default() };
        process_stereo_prepared(&mut f, sine(100.0, 0.1), 8, &tr);
        let mut m = [0.0; 3];
        f.meter(&mut m);
        assert!((m[0] / 1000.0 - 1.0).abs() < 0.05, "{}", m[0]);
        let tr = Transport { playing: true, bpm: 120.0, position_beats: 3.0, ..Transport::default() };
        process_stereo_prepared(&mut f, sine(100.0, 0.1), 8, &tr);
        f.meter(&mut m);
        assert!((m[0] / (1000.0 * 2f32.powf(0.5)) - 1.0).abs() < 0.05, "{}", m[0]);
    }

    #[test]
    fn envelope_opens_the_filter() {
        let mut f = Filter::new();
        set_plain(&mut f, p::CUTOFF, 200.0);
        set_plain(&mut f, p::ENV_AMOUNT, 3.0);
        process_stereo(&mut f, sine(100.0, 0.9), 4800);
        let mut m = [0.0; 3];
        f.meter(&mut m);
        assert!(m[0] > 800.0, "{}", m[0]);
    }
}
