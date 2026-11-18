//! `mod.envfollow` — envelope follower modulator. It measures the track's
//! audio at its place in the chain (peak or RMS, with rise and fall times)
//! and passes it through untouched; the engine adds the measured level, scaled
//! by each mapping's range, to the parameters it drives. The level of one
//! block steers the next, so the control lags by a block.
//!
//! The output is `depth · level · gain` clamped to 0..1 (`Invert` flips it).
//!
//! Meter layout: `[output (0..1), then HISTORY outputs, newest last, one per block]`.

use crate::common::{ParamStore, load_state, save_state, write_meter};
use aitrack_dsp::envelope::{DetectorMode, EnvelopeFollower};
use aitrack_dsp::{Category, ModBlock, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "mod.envfollow";

pub mod p {
    pub const GAIN: u32 = 0;
    pub const RISE: u32 = 1;
    pub const FALL: u32 = 2;
    pub const MODE: u32 = 3;
    pub const DEPTH: u32 = 4;
    pub const INVERT: u32 = 5;
}

const MODES: &[&str] = &["Peak", "RMS"];
const OFF_ON: &[&str] = &["Off", "On"];
/// Outputs kept for the display.
pub const HISTORY: usize = 96;

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::GAIN, "Gain", -24.0, 48.0, 12.0).unit("dB"),
        ParamInfo::new(p::RISE, "Rise", 0.1, 500.0, 5.0).unit("ms").log(),
        ParamInfo::new(p::FALL, "Fall", 5.0, 5000.0, 150.0).unit("ms").log(),
        ParamInfo::new(p::MODE, "Mode", 0.0, 1.0, 0.0).choices(MODES),
        ParamInfo::new(p::DEPTH, "Depth", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::INVERT, "Invert", 0.0, 1.0, 0.0).choices(OFF_ON),
    ]
}

pub struct EnvFollow {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    env: EnvelopeFollower,
    out: f32,
    history: [f32; HISTORY],
}

pub fn create() -> Box<dyn Processor> {
    Box::new(EnvFollow::new())
}

impl Default for EnvFollow {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvFollow {
    pub fn new() -> Self {
        let params = params();
        let mut c = Self {
            info: PluginInfo {
                id: ID,
                name: "Envelope Follower",
                vendor: "AI-Track",
                category: Category::Utility,
                tags: &["modulation", "envelope", "follower", "modulator", "mapping", "peak controller"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            env: EnvelopeFollower::new(DetectorMode::Peak),
            out: 0.0,
            history: [0.0; HISTORY],
        };
        c.prepare(48_000.0, 512);
        c
    }

    fn output(&self, level: f32) -> f32 {
        let v = (level * db_to_gain(self.params.plain(p::GAIN)) * self.params.plain(p::DEPTH) / 100.0).clamp(0.0, 1.0);
        if self.params.on(p::INVERT) { 1.0 - v } else { v }
    }
}

impl Processor for EnvFollow {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        self.env.set_rms_window(10.0, sample_rate);
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        self.env.reset();
        self.out = self.output(0.0);
        self.history = [0.0; HISTORY];
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        if self.params.take_dirty() {
            self.env.set_mode(if self.params.choice(p::MODE) == 1 { DetectorMode::Rms } else { DetectorMode::Peak });
            self.env.set_times(self.params.plain(p::RISE), self.params.plain(p::FALL), self.sr);
        }
        let buf = &*ctx.buffer;
        let frames = buf.frames();
        let (l, r) = if buf.channels() >= 2 { (buf.channel(0), buf.channel(1)) } else { (buf.channel(0), buf.channel(0)) };
        for i in 0..frames {
            let (a, b) = (l[i], r[i]);
            self.env.process(if a.abs() >= b.abs() { a } else { b });
        }
        self.out = self.output(self.env.value());
        self.history.copy_within(1.., 0);
        self.history[HISTORY - 1] = self.out;
    }

    fn modulate(&mut self, _m: &ModBlock<'_>, out: &mut [f32]) -> bool {
        // Re-derived so a knob turn is heard before the next block arrives.
        out.fill(self.output(self.env.value()));
        true
    }

    fn tail(&self) -> u32 {
        (self.params.plain(p::FALL) as f64 * 0.006 * self.sr) as u32
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
        let o = write_meter(out, 0, &[self.out]);
        write_meter(out, o, &self.history)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, process_stereo_prepared, set_plain};
    use aitrack_dsp::Transport;

    fn modulate(p: &mut EnvFollow) -> f32 {
        let m = ModBlock { sample_rate: SR, playing: true, bpm: 120.0, step: 32, frames: 128, start_samples: 0, beats: &[0.0; 4], events: &[] };
        let mut out = [0.0; 4];
        assert!(p.modulate(&m, &mut out));
        assert!(out.iter().all(|x| *x == out[0]));
        out[0]
    }

    #[test]
    fn follows_the_level_and_passes_audio_through() {
        let mut p = EnvFollow::new();
        p.prepare(SR, 128);
        set_plain(&mut p, p::GAIN, 0.0);
        set_plain(&mut p, p::RISE, 1.0);
        set_plain(&mut p, p::FALL, 50.0);
        assert_eq!(modulate(&mut p), 0.0, "silent before anything plays");
        let x = |i: usize| (0.5 * (i as f32 * 0.05).sin(), 0.5 * (i as f32 * 0.05).sin());
        let (l, _) = process_stereo_prepared(&mut p, x, 4800, &Transport::default());
        assert_eq!(l[100], 0.5 * (100f32 * 0.05).sin(), "audio untouched");
        let loud = modulate(&mut p);
        assert!((0.4..=0.55).contains(&loud), "{loud}");
        process_stereo_prepared(&mut p, |_| (0.0, 0.0), 24_000, &Transport::default());
        assert!(modulate(&mut p) < 0.01, "falls back");
    }

    #[test]
    fn gain_depth_and_invert_shape_the_output() {
        let mut p = EnvFollow::new();
        p.prepare(SR, 128);
        set_plain(&mut p, p::GAIN, 0.0);
        process_stereo_prepared(&mut p, |i| (0.2 * (i as f32 * 0.05).sin(), 0.0), 4800, &Transport::default());
        let base = modulate(&mut p);
        set_plain(&mut p, p::GAIN, 12.0);
        assert!(modulate(&mut p) > base * 3.0, "+12 dB is about four times");
        set_plain(&mut p, p::DEPTH, 50.0);
        let half = modulate(&mut p);
        set_plain(&mut p, p::INVERT, 1.0);
        assert!((modulate(&mut p) - (1.0 - half)).abs() < 1e-6);
    }

    #[test]
    fn meter_fits_and_tail_follows_fall() {
        let mut p = EnvFollow::new();
        let mut m = [0.0f32; 320];
        assert_eq!(p.meter(&mut m), 1 + HISTORY);
        set_plain(&mut p, p::FALL, 1000.0);
        assert!(p.tail() > 5 * 48_000 / 2);
    }
}
