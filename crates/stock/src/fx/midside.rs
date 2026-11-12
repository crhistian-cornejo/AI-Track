//! `fx.midside` — mid/side encoder, gain stage and decoder.
//!
//! `M = (L + R) / 2`, `S = (L − R) / 2`; the decoder is `L = M + S`,
//! `R = M − S`. *Mid* and *Side* set the two gains, *Width* scales the side
//! signal (100 % is neutral, 0 % mono, 200 % doubles the stereo difference).
//!
//! Solo switches listen to one component: *Solo Mid* removes the side signal
//! (mono), *Solo Side* removes the mid signal (difference only). With both
//! solos on the signal passes through normally.
//!
//! Meter layout: `[mid peak, side peak]` after the gains, before the solos.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.midside";

pub mod p {
    pub const MID: u32 = 0;
    pub const SIDE: u32 = 1;
    pub const SOLO_MID: u32 = 2;
    pub const SOLO_SIDE: u32 = 3;
    pub const WIDTH: u32 = 4;
}

const OFF_ON: &[&str] = &["Off", "On"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::MID, "Mid Gain", -24.0, 24.0, 0.0).unit("dB").short("Mid"),
        ParamInfo::new(p::SIDE, "Side Gain", -24.0, 24.0, 0.0).unit("dB").short("Side"),
        ParamInfo::new(p::SOLO_MID, "Solo Mid", 0.0, 1.0, 0.0).choices(OFF_ON).short("M Solo"),
        ParamInfo::new(p::SOLO_SIDE, "Solo Side", 0.0, 1.0, 0.0).choices(OFF_ON).short("S Solo"),
        ParamInfo::new(p::WIDTH, "Width", 0.0, 200.0, 100.0).unit("%"),
    ]
}

pub struct MidSide {
    info: PluginInfo,
    params: ParamStore,
    mid: Smoother,
    side: Smoother,
    width: Smoother,
    solo_mid: bool,
    solo_side: bool,
    peaks: [f32; 2],
    first: bool,
    sr: f64,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(MidSide::new())
}

impl Default for MidSide {
    fn default() -> Self {
        Self::new()
    }
}

impl MidSide {
    pub fn new() -> Self {
        let params = params();
        let mut m = Self {
            info: PluginInfo {
                id: ID,
                name: "Mid/Side",
                vendor: "AI-Track",
                category: Category::Utility,
                tags: &["mid-side", "stereo", "width", "utility"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            mid: Smoother::new(1.0),
            side: Smoother::new(1.0),
            width: Smoother::new(1.0),
            solo_mid: false,
            solo_side: false,
            peaks: [0.0; 2],
            first: true,
            sr: 48_000.0,
        };
        m.prepare(48_000.0, 512);
        m
    }

    fn update(&mut self) {
        self.mid.set_target(db_to_gain(self.params.plain(p::MID)));
        self.side.set_target(db_to_gain(self.params.plain(p::SIDE)));
        self.width.set_target(self.params.plain(p::WIDTH) / 100.0);
        self.solo_mid = self.params.on(p::SOLO_MID);
        self.solo_side = self.params.on(p::SOLO_SIDE);
        if self.first {
            self.mid.snap_to_target();
            self.side.snap_to_target();
            self.width.snap_to_target();
            self.first = false;
        }
    }
}

impl Segmented for MidSide {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let decay = (-1.0f32 / (0.3f32 * self.sr as f32)).exp();
        let this = &mut *self;
        let mut pk = this.peaks;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let mg = this.mid.next();
            let sg = this.side.next();
            let width = this.width.next();
            let mid = (l + r) * 0.5 * mg;
            let side = (l - r) * 0.5 * sg * width;
            pk[0] = (pk[0] * decay).max(mid.abs());
            pk[1] = (pk[1] * decay).max(side.abs());
            let (m, s) = match (this.solo_mid, this.solo_side) {
                (true, false) => (mid, 0.0),
                (false, true) => (0.0, side),
                _ => (mid, side),
            };
            (sanitize(m + s), sanitize(m - s))
        });
        self.peaks = pk;
    }
}

impl Processor for MidSide {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in [&mut self.mid, &mut self.side, &mut self.width] {
            s.set_time(20.0, sample_rate);
        }
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        self.peaks = [0.0; 2];
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
        write_meter(out, 0, &[gain_to_db(self.peaks[0]), gain_to_db(self.peaks[1])])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{peak, process_stereo, rms, set_plain, sine};

    fn neutral(m: &mut MidSide) {
        set_plain(m, p::MID, 0.0);
        set_plain(m, p::SIDE, 0.0);
        set_plain(m, p::SOLO_MID, 0.0);
        set_plain(m, p::SOLO_SIDE, 0.0);
        set_plain(m, p::WIDTH, 100.0);
    }

    #[test]
    fn neutral_is_a_round_trip() {
        let mut m = MidSide::new();
        neutral(&mut m);
        let mut seed = 0xabcd_1234u32;
        let src: Vec<(f32, f32)> = (0..8192)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let a = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let b = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                (a, b)
            })
            .collect();
        let src2 = src.clone();
        let (l, r) = process_stereo(&mut m, move |i| src2[i], 8192);
        let mut worst = 0.0f32;
        for i in 0..8192 {
            worst = worst.max((l[i] - src[i].0).abs()).max((r[i] - src[i].1).abs());
        }
        assert!(worst < 1e-6, "{worst}");
    }

    #[test]
    fn width_zero_folds_to_mono_and_two_widens() {
        let mut m = MidSide::new();
        neutral(&mut m);
        set_plain(&mut m, p::WIDTH, 0.0);
        let (l, r) = process_stereo(&mut m, |i| {
            let v = sine(440.0, 0.2)(i).0;
            (v, -v)
        }, 4800);
        let mut worst = 0.0f32;
        for i in 1000..4800 {
            worst = worst.max((l[i] - r[i]).abs());
        }
        assert!(worst < 1e-6, "{worst}");

        // An out-of-phase input has only a side component, so width scales it.
        let gain = |width: f32| {
            let mut m = MidSide::new();
            neutral(&mut m);
            set_plain(&mut m, p::WIDTH, width);
            let (l, _) = process_stereo(&mut m, |i| {
                let v = sine(440.0, 0.2)(i).0;
                (v, -v)
            }, 4800);
            gain_to_db(rms(&l[1000..]) / (0.2 / std::f32::consts::SQRT_2))
        };
        assert!((gain(200.0) - 6.02).abs() < 0.1, "{}", gain(200.0));
        assert!(gain(50.0) < -5.9, "{}", gain(50.0));
    }

    #[test]
    fn mid_and_side_gains_are_independent() {
        let mid_gain = |db: f32| {
            let mut m = MidSide::new();
            neutral(&mut m);
            set_plain(&mut m, p::MID, db);
            let (l, _) = process_stereo(&mut m, sine(440.0, 0.1), 4800);
            gain_to_db(rms(&l[1000..]) / (0.1 / std::f32::consts::SQRT_2))
        };
        assert!((mid_gain(6.0) - 6.02).abs() < 0.1, "{}", mid_gain(6.0));
        assert!(mid_gain(0.0).abs() < 0.1, "{}", mid_gain(0.0));
    }

    #[test]
    fn solos_isolate_the_components() {
        // Mid solo on a centred sine keeps it, side solo silences it.
        let mut mid = MidSide::new();
        neutral(&mut mid);
        set_plain(&mut mid, p::SOLO_MID, 1.0);
        let (l, r) = process_stereo(&mut mid, sine(440.0, 0.2), 4800);
        assert!(peak(&l[1000..]) > 0.19 && peak(&r[1000..]) > 0.19);

        let mut side = MidSide::new();
        neutral(&mut side);
        set_plain(&mut side, p::SOLO_SIDE, 1.0);
        let (l, r) = process_stereo(&mut side, sine(440.0, 0.2), 4800);
        assert!(peak(&l[1000..]) < 1e-6 && peak(&r[1000..]) < 1e-6);

        // Side solo on an out-of-phase input gives L = -R.
        let mut side = MidSide::new();
        neutral(&mut side);
        set_plain(&mut side, p::SOLO_SIDE, 1.0);
        let (l, r) = process_stereo(&mut side, |i| {
            let v = sine(440.0, 0.2)(i).0;
            (v, -v)
        }, 4800);
        let mut worst = 0.0f32;
        for i in 1000..4800 {
            worst = worst.max((l[i] + r[i]).abs());
        }
        assert!(worst < 1e-6, "{worst}");

        // Both solos on leave the signal alone.
        let mut both = MidSide::new();
        neutral(&mut both);
        set_plain(&mut both, p::SOLO_MID, 1.0);
        set_plain(&mut both, p::SOLO_SIDE, 1.0);
        let (l, _) = process_stereo(&mut both, sine(440.0, 0.2), 4800);
        assert!(peak(&l[1000..]) > 0.19);
    }

    #[test]
    fn width_keeps_mid_content() {
        let mut m = MidSide::new();
        neutral(&mut m);
        set_plain(&mut m, p::WIDTH, 0.0);
        let (l, _) = process_stereo(&mut m, sine(440.0, 0.2), 4800);
        let gain = gain_to_db(rms(&l[1000..]) / (0.2 / std::f32::consts::SQRT_2));
        assert!(gain.abs() < 0.1, "{gain}");
    }

    #[test]
    fn meter_reports_mid_and_side() {
        let mut m = MidSide::new();
        neutral(&mut m);
        process_stereo(&mut m, |i| {
            let v = sine(440.0, 0.2)(i).0;
            (v, -v)
        }, 4800);
        let mut meter = [0.0f32; 2];
        assert_eq!(m.meter(&mut meter), 2);
        assert!(meter[0] < -40.0, "mid {}", meter[0]);
        assert!(meter[1] > -15.0, "side {}", meter[1]);
    }

    #[test]
    fn state_round_trip() {
        let mut m = MidSide::new();
        neutral(&mut m);
        set_plain(&mut m, p::SIDE, -4.0);
        let bytes = m.save_state();
        let mut n = MidSide::new();
        n.load_state(&bytes);
        assert!((n.params.plain(p::SIDE) + 4.0).abs() < 1e-4);
    }
}
