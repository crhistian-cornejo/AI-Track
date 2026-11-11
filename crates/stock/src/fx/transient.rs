//! `fx.transient` — level-independent attack / sustain shaper.
//!
//! Two pairs of envelope followers in dB:
//! * **attack** = fast-attack envelope − slow-attack envelope (≥ 0): large at
//!   onsets, zero once the slow follower catches up (its attack time is the
//!   *Attack Window*);
//! * **sustain** = slow-release envelope − fast-release envelope (≥ 0):
//!   grows in the decay tail (its release time is the *Sustain Length*).
//!
//! The applied gain is `attack% · attack + sustain% · sustain` (dB, ±24 dB
//! cap), so the result does not depend on the input level. Optional output
//! clip (ADAA hard clip at 0 dBFS) and dry/wet mix. Zero latency.
//!
//! Meter layout: `[0]` gain currently applied, dB.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use aitrack_dsp::shaper::{Adaa1, ShaperKind};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.transient";

pub mod p {
    pub const ATTACK: u32 = 0;
    pub const SUSTAIN: u32 = 1;
    pub const WINDOW: u32 = 2;
    pub const LENGTH: u32 = 3;
    pub const OUTPUT: u32 = 4;
    pub const CLIP: u32 = 5;
    pub const MIX: u32 = 6;
}

const OFF_ON: &[&str] = &["Off", "On"];
const MAX_DB: f32 = 24.0;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::ATTACK, "Attack", -100.0, 100.0, 0.0).unit("%").short("Att"),
        ParamInfo::new(p::SUSTAIN, "Sustain", -100.0, 100.0, 0.0).unit("%").short("Sus"),
        ParamInfo::new(p::WINDOW, "Attack Window", 1.0, 60.0, 15.0).unit("ms").log().short("Window"),
        ParamInfo::new(p::LENGTH, "Sustain Length", 20.0, 1_000.0, 200.0).unit("ms").log().short("Length"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 24.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::CLIP, "Output Clip", 0.0, 1.0, 0.0).choices(OFF_ON).short("Clip"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
    ]
}

pub struct Transient {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    fast_a: EnvelopeFollower,
    slow_a: EnvelopeFollower,
    fast_r: EnvelopeFollower,
    slow_r: EnvelopeFollower,
    attack: f32,
    sustain: f32,
    clip: bool,
    clippers: [Adaa1; 2],
    gain_db: Smoother,
    output: Smoother,
    mix: Smoother,
    last_db: f32,
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Transient::new())
}

impl Default for Transient {
    fn default() -> Self {
        Self::new()
    }
}

impl Transient {
    pub fn new() -> Self {
        let params = params();
        let mut t = Self {
            info: PluginInfo {
                id: ID,
                name: "Transient Shaper",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["dynamics", "transient", "attack", "sustain"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            fast_a: EnvelopeFollower::new(DetectorMode::Peak),
            slow_a: EnvelopeFollower::new(DetectorMode::Peak),
            fast_r: EnvelopeFollower::new(DetectorMode::Peak),
            slow_r: EnvelopeFollower::new(DetectorMode::Peak),
            attack: 0.0,
            sustain: 0.0,
            clip: false,
            clippers: [Adaa1::new(ShaperKind::HardClip); 2],
            gain_db: Smoother::new(0.0),
            output: Smoother::new(1.0),
            mix: Smoother::new(1.0),
            last_db: 0.0,
            first: true,
        };
        t.prepare(48_000.0, 512);
        t
    }

    fn update(&mut self) {
        let ps = &self.params;
        let sr = self.sr;
        let window = ps.plain(p::WINDOW);
        let length = ps.plain(p::LENGTH);
        self.fast_a.set_times(0.3, 60.0, sr);
        self.slow_a.set_times(window, 60.0, sr);
        self.fast_r.set_times(0.3, 25.0, sr);
        self.slow_r.set_times(0.3, length, sr);
        self.attack = ps.plain(p::ATTACK) / 100.0;
        self.sustain = ps.plain(p::SUSTAIN) / 100.0;
        self.clip = ps.on(p::CLIP);
        self.output.set_target(db_to_gain(ps.plain(p::OUTPUT)));
        self.mix.set_target(ps.plain(p::MIX) / 100.0);
        if self.first {
            self.output.snap_to_target();
            self.mix.snap_to_target();
            self.first = false;
        }
    }
}

impl Segmented for Transient {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let this = &mut *self;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let d = l.abs().max(r.abs());
            let fa = gain_to_db(this.fast_a.process(d));
            let sa = gain_to_db(this.slow_a.process(d));
            let fr = gain_to_db(this.fast_r.process(d));
            let sr = gain_to_db(this.slow_r.process(d));
            // Below −90 dB there is nothing to shape.
            let (att, sus) = if fa > -90.0 { ((fa - sa).clamp(0.0, 60.0), (sr - fr).clamp(0.0, 60.0)) } else { (0.0, 0.0) };
            let target = (this.attack * att + this.sustain * sus).clamp(-MAX_DB, MAX_DB);
            this.gain_db.set_target(target);
            let gdb = this.gain_db.next();
            this.last_db = gdb;
            let g = db_to_gain(gdb) * this.output.next();
            let mix = this.mix.next();
            let mut wl = l * g;
            let mut wr = r * g;
            if this.clip {
                wl = this.clippers[0].process(wl);
                wr = this.clippers[1].process(wr);
            }
            (sanitize(l + (wl - l) * mix), sanitize(r + (wr - r) * mix))
        });
    }
}

impl Processor for Transient {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.gain_db.set_time(1.0, sample_rate);
        self.output.set_time(20.0, sample_rate);
        self.mix.set_time(20.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for e in [&mut self.fast_a, &mut self.slow_a, &mut self.fast_r, &mut self.slow_r] {
            e.reset();
        }
        for c in &mut self.clippers {
            c.reset();
        }
        self.gain_db.snap(0.0);
        self.last_db = 0.0;
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

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[self.last_db])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{peak, process_stereo, set_plain};

    /// Repeating decaying hits: sharp onset, 150 ms tail.
    fn hits(amp: f32) -> impl FnMut(usize) -> (f32, f32) {
        move |i| {
            let t = (i % 12_000) as f32 / 48_000.0;
            let v = amp * (-t * 20.0).exp() * (std::f32::consts::TAU * 180.0 * t).sin();
            (v, v)
        }
    }

    fn onset_tail(x: &[f32]) -> (f32, f32) {
        // Third hit: onset region and late tail.
        let base = 24_000;
        (peak(&x[base..base + 480]), peak(&x[base + 6000..base + 7000]))
    }

    #[test]
    fn neutral_by_default() {
        let mut t = Transient::new();
        let (l, _) = process_stereo(&mut t, hits(0.5), 36_000);
        let mut src = hits(0.5);
        for (i, v) in l.iter().enumerate() {
            assert!((v - src(i).0).abs() < 1e-5);
        }
    }

    #[test]
    fn attack_boosts_onsets_and_sustain_tails_level_independent() {
        let dry = {
            let mut src = hits(0.5);
            let x: Vec<f32> = (0..36_000).map(|i| src(i).0).collect();
            onset_tail(&x)
        };
        for amp in [0.5f32, 0.005] {
            let mut t = Transient::new();
            set_plain(&mut t, p::ATTACK, 100.0);
            let (l, _) = process_stereo(&mut t, hits(amp), 36_000);
            let (on, _) = onset_tail(&l);
            let boost = gain_to_db(on / (dry.0 * amp / 0.5));
            assert!(boost > 3.0, "amp {amp}: onset boost {boost}");

            let mut t = Transient::new();
            set_plain(&mut t, p::SUSTAIN, 100.0);
            let (l, _) = process_stereo(&mut t, hits(amp), 36_000);
            let (_, tail) = onset_tail(&l);
            let lift = gain_to_db(tail / (dry.1 * amp / 0.5));
            assert!(lift > 3.0, "amp {amp}: tail lift {lift}");
        }
    }

    #[test]
    fn clip_limits_output() {
        let mut t = Transient::new();
        set_plain(&mut t, p::OUTPUT, 12.0);
        set_plain(&mut t, p::CLIP, 1.0);
        let (l, _) = process_stereo(&mut t, hits(0.9), 24_000);
        assert!(peak(&l) <= 1.0 + 1e-3);
    }
}
