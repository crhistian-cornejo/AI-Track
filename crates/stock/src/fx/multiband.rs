//! `fx.multiband` — three-band utility crossover with per-band gain, mute,
//! solo and delta, plus a global bypass.
//!
//! The default split is an LR4 (`Crossover3`): the bands sum to a 2nd-order
//! all-pass, so the magnitude is flat and the band faders are frequency
//! selective without comb filtering. *Linear* mode replaces it with the
//! complementary linear-phase FIR split (`LinearPhaseCrossover3`): the bands
//! sum to the input delayed by the FIR's half length, exactly. The reported
//! latency follows the mode, and the bypass path is delayed to match it.
//!
//! *Delta* on a band outputs that band after gain minus the neutral band, so
//! only what the band fader changes remains. *Solo* overrides *Mute*: when any
//! band is soloed only the soloed bands are heard.
//!
//! Meter layout: `[low peak, mid peak, high peak]` before the faders.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::crossover::{Crossover3, LinearPhaseCrossover3, linear_phase_taps};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain, gain_to_db};

pub const ID: &str = "fx.multiband";

pub mod p {
    pub const XOVER_LOW: u32 = 0;
    pub const XOVER_HIGH: u32 = 1;
    pub const MODE: u32 = 2;
    pub const BYPASS: u32 = 3;
    /// First per-band id; band `b` uses `BAND0 + b * BAND_STRIDE + offset`.
    pub const BAND0: u32 = 4;
    pub const BAND_STRIDE: u32 = 4;
    pub const GAIN: u32 = 0;
    pub const MUTE: u32 = 1;
    pub const SOLO: u32 = 2;
    pub const DELTA: u32 = 3;

    /// Id of `offset` (GAIN, MUTE…) for band `b` (0..3).
    pub const fn band(b: u32, offset: u32) -> u32 {
        BAND0 + b * BAND_STRIDE + offset
    }
}

/// Split modes.
pub mod mode {
    pub const MINIMUM: f32 = 0.0;
    pub const LINEAR: f32 = 1.0;
}

const MODES: &[&str] = &["Minimum", "Linear"];
const OFF_ON: &[&str] = &["Off", "On"];
const GAIN_NAMES: [&str; 3] = ["Low Gain", "Mid Gain", "High Gain"];
const MUTE_NAMES: [&str; 3] = ["Low Mute", "Mid Mute", "High Mute"];
const SOLO_NAMES: [&str; 3] = ["Low Solo", "Mid Solo", "High Solo"];
const DELTA_NAMES: [&str; 3] = ["Low Delta", "Mid Delta", "High Delta"];

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::XOVER_LOW, "Crossover Low", 40.0, 2000.0, 200.0).unit("Hz").log().short("X Low"),
        ParamInfo::new(p::XOVER_HIGH, "Crossover High", 500.0, 12_000.0, 2500.0).unit("Hz").log().short("X High"),
        ParamInfo::new(p::MODE, "Mode", 0.0, 1.0, mode::MINIMUM).choices(MODES),
        ParamInfo::new(p::BYPASS, "Bypass", 0.0, 1.0, 0.0).choices(OFF_ON),
    ];
    for b in 0..3u32 {
        let i = b as usize;
        v.push(ParamInfo::new(p::band(b, p::GAIN), GAIN_NAMES[i], -24.0, 24.0, 0.0).unit("dB").short("Gain"));
        v.push(ParamInfo::new(p::band(b, p::MUTE), MUTE_NAMES[i], 0.0, 1.0, 0.0).choices(OFF_ON).short("Mute"));
        v.push(ParamInfo::new(p::band(b, p::SOLO), SOLO_NAMES[i], 0.0, 1.0, 0.0).choices(OFF_ON).short("Solo"));
        v.push(ParamInfo::new(p::band(b, p::DELTA), DELTA_NAMES[i], 0.0, 1.0, 0.0).choices(OFF_ON).short("Delta"));
    }
    v
}

pub struct Multiband {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    min: [Crossover3; 2],
    lin: [LinearPhaseCrossover3; 2],
    dry: [DelayLine; 2],
    gain: [Smoother; 3],
    mute: [bool; 3],
    solo: [bool; 3],
    delta: [bool; 3],
    wet: Smoother,
    linear: bool,
    latency: usize,
    peaks: [f32; 3],
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Multiband::new())
}

impl Default for Multiband {
    fn default() -> Self {
        Self::new()
    }
}

impl Multiband {
    pub fn new() -> Self {
        let params = params();
        let mut m = Self {
            info: PluginInfo {
                id: ID,
                name: "Multiband",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["multiband", "crossover", "utility", "master"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            min: [Crossover3::default(); 2],
            lin: [LinearPhaseCrossover3::default(), LinearPhaseCrossover3::default()],
            dry: [DelayLine::new(8), DelayLine::new(8)],
            gain: [Smoother::new(1.0); 3],
            mute: [false; 3],
            solo: [false; 3],
            delta: [false; 3],
            wet: Smoother::new(1.0),
            linear: false,
            latency: 0,
            peaks: [0.0; 3],
            first: true,
        };
        m.prepare(48_000.0, 512);
        m
    }

    fn update(&mut self) {
        let linear = self.params.choice(p::MODE) == 1;
        if linear != self.linear {
            self.linear = linear;
            // Drop the stale history of the splitter that becomes active.
            if linear {
                for c in &mut self.lin {
                    c.reset();
                }
            } else {
                for c in &mut self.min {
                    c.reset();
                }
            }
        }
        self.latency = if linear { self.lin[0].latency() } else { 0 };
        let (f1, f2) = (self.params.plain(p::XOVER_LOW), self.params.plain(p::XOVER_HIGH));
        let f2 = f2.max(f1 * 1.2);
        for c in 0..2 {
            self.min[c].set(3, f1, f2, self.sr);
            self.lin[c].set(3, f1, f2, self.sr);
        }
        for b in 0..3u32 {
            let i = b as usize;
            self.gain[i].set_target(db_to_gain(self.params.plain(p::band(b, p::GAIN))));
            self.mute[i] = self.params.on(p::band(b, p::MUTE));
            self.solo[i] = self.params.on(p::band(b, p::SOLO));
            self.delta[i] = self.params.on(p::band(b, p::DELTA));
        }
        self.wet.set_target(if self.params.on(p::BYPASS) { 0.0 } else { 1.0 });
        if self.first {
            for g in &mut self.gain {
                g.snap_to_target();
            }
            self.wet.snap_to_target();
            self.first = false;
        }
    }
}

impl Segmented for Multiband {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let linear = self.linear;
        let lat = self.latency;
        let any_solo = self.solo.iter().any(|&s| s);
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let this = &mut *self;
        let mut pk = this.peaks;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let wet = this.wet.next();
            let mut g = [0.0f32; 3];
            for b in 0..3 {
                g[b] = this.gain[b].next();
            }
            let mut out = [l, r];
            let mut band_pk = [0.0f32; 3];
            for c in 0..2 {
                let x = out[c];
                let mut split = [0.0f32; 3];
                if linear {
                    this.lin[c].split(x, &mut split);
                } else {
                    this.min[c].split(x, &mut split);
                }
                let mut acc = 0.0;
                for b in 0..3 {
                    let raw = split[b];
                    band_pk[b] = band_pk[b].max(raw.abs());
                    let audible = if any_solo { this.solo[b] } else { !this.mute[b] };
                    if !audible {
                        continue;
                    }
                    let processed = raw * g[b];
                    acc += if this.delta[b] { processed - raw } else { processed };
                }
                this.dry[c].push(x);
                let dry = this.dry[c].read(lat);
                out[c] = sanitize(dry + (acc - dry) * wet);
            }
            for b in 0..3 {
                pk[b] = (pk[b] * decay).max(band_pk[b]);
            }
            (out[0], out[1])
        });
        self.peaks = pk;
    }
}

impl Processor for Multiband {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for g in &mut self.gain {
            g.set_time(20.0, sample_rate);
        }
        self.wet.set_time(20.0, sample_rate);
        let f1 = self.params.plain(p::XOVER_LOW);
        let f2 = self.params.plain(p::XOVER_HIGH).max(f1 * 1.2);
        self.min = [Crossover3::default(); 2];
        self.lin = [LinearPhaseCrossover3::new(3, f1, f2, sample_rate), LinearPhaseCrossover3::new(3, f1, f2, sample_rate)];
        let max_lat = (linear_phase_taps(sample_rate) - 1) / 2;
        self.dry = [DelayLine::new(max_lat + 8), DelayLine::new(max_lat + 8)];
        self.linear = false;
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for c in &mut self.min {
            c.reset();
        }
        for c in &mut self.lin {
            c.reset();
        }
        for d in &mut self.dry {
            d.clear();
        }
        self.peaks = [0.0; 3];
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
        if self.params.choice(p::MODE) == 1 { LinearPhaseCrossover3::latency_for(self.sr) as u32 } else { 0 }
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &[gain_to_db(self.peaks[0]), gain_to_db(self.peaks[1]), gain_to_db(self.peaks[2])])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, peak, process_stereo, rms, set_plain, sine};

    fn neutral(m: &mut Multiband) {
        for b in 0..3 {
            set_plain(m, p::band(b, p::GAIN), 0.0);
            set_plain(m, p::band(b, p::MUTE), 0.0);
            set_plain(m, p::band(b, p::SOLO), 0.0);
            set_plain(m, p::band(b, p::DELTA), 0.0);
        }
        set_plain(m, p::BYPASS, 0.0);
    }

    #[test]
    fn neutral_minimum_phase_sum_is_flat() {
        for f in [60.0, 230.0, 900.0, 2500.0, 6000.0, 14_000.0] {
            let mut m = Multiband::new();
            neutral(&mut m);
            let amp = 0.2;
            let (l, _) = process_stereo(&mut m, sine(f, amp), 24_000);
            let skip = 8_000;
            let gain = gain_to_db(rms(&l[skip..]) / (amp / std::f32::consts::SQRT_2));
            assert!(gain.abs() < 0.1, "{f} Hz: {gain} dB");
        }
    }

    #[test]
    fn neutral_sum_of_noise_is_flat() {
        let mut m = Multiband::new();
        neutral(&mut m);
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let (l, _) = process_stereo(&mut m, move |_| (noise() * 0.2, 0.0), 48_000);
        // Rebuild the same noise to compare levels.
        let mut seed = 0x1234_5678u32;
        let mut input = Vec::with_capacity(48_000);
        for _ in 0..48_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            input.push(((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.2);
        }
        let skip = 8_000;
        let gain = gain_to_db(rms(&l[skip..]) / rms(&input[skip..]));
        assert!(gain.abs() < 0.05, "{gain} dB");
    }

    #[test]
    fn linear_mode_sums_to_the_delayed_input() {
        let mut m = Multiband::new();
        neutral(&mut m);
        set_plain(&mut m, p::MODE, mode::LINEAR);
        assert!(m.latency() > 0);
        let amp = 0.2;
        let n = 24_000;
        let (l, _) = process_stereo(&mut m, sine(440.0, amp), n);
        let lat = m.latency() as usize;
        let mut worst = 0.0f32;
        for i in lat + 200..n {
            let expect = amp * (std::f64::consts::TAU * 440.0 * (i - lat) as f64 / SR).sin() as f32;
            worst = worst.max((l[i] - expect).abs());
        }
        assert!(worst < 1e-4, "latency {lat}: {worst}");
    }

    #[test]
    fn bypass_is_the_delayed_input() {
        for mode in [mode::MINIMUM, mode::LINEAR] {
            let mut m = Multiband::new();
            neutral(&mut m);
            set_plain(&mut m, p::MODE, mode);
            set_plain(&mut m, p::BYPASS, 1.0);
            set_plain(&mut m, p::band(0, p::GAIN), 12.0);
            let n = 12_000;
            let amp = 0.15;
            let (l, _) = process_stereo(&mut m, sine(700.0, amp), n);
            let lat = m.latency() as usize;
            let mut worst = 0.0f32;
            for i in lat + 200..n {
                let expect = amp * (std::f64::consts::TAU * 700.0 * (i - lat) as f64 / SR).sin() as f32;
                worst = worst.max((l[i] - expect).abs());
            }
            assert!(worst < 1e-4, "mode {mode}: {worst}");
        }
    }

    #[test]
    fn mute_and_solo_select_bands() {
        // Solo one band at a time: it passes its own range and rejects the
        // others (an LR4 band is 24 dB/octave away from its edges).
        let gain_at = |solo: u32, freq: f32| {
            let mut m = Multiband::new();
            neutral(&mut m);
            set_plain(&mut m, p::band(solo, p::SOLO), 1.0);
            let (l, _) = process_stereo(&mut m, sine(freq, 0.2), 24_000);
            gain_to_db(rms(&l[12_000..]) / (0.2 / std::f32::consts::SQRT_2))
        };
        assert!(gain_at(0, 80.0).abs() < 0.3, "{}", gain_at(0, 80.0));
        assert!(gain_at(2, 10_000.0).abs() < 0.3, "{}", gain_at(2, 10_000.0));
        assert!(gain_at(1, 10_000.0) < -40.0, "{}", gain_at(1, 10_000.0));
        assert!(gain_at(1, 80.0) < -30.0, "{}", gain_at(1, 80.0));

        // Muting every band is silent.
        let mut m = Multiband::new();
        neutral(&mut m);
        for b in 0..3 {
            set_plain(&mut m, p::band(b, p::MUTE), 1.0);
        }
        let (l, _) = process_stereo(&mut m, sine(440.0, 0.2), 4800);
        assert!(peak(&l[1000..]) < 1e-6);
    }

    #[test]
    fn band_gain_changes_only_its_band() {
        let low = |db: f32| {
            let mut m = Multiband::new();
            neutral(&mut m);
            set_plain(&mut m, p::band(0, p::GAIN), db);
            let (l, _) = process_stereo(&mut m, sine(80.0, 0.1), 24_000);
            gain_to_db(rms(&l[12_000..]) / (0.1 / std::f32::consts::SQRT_2))
        };
        assert!((low(6.0) - 6.0).abs() < 0.2, "{}", low(6.0));
        assert!(low(0.0).abs() < 0.1, "{}", low(0.0));
    }

    #[test]
    fn delta_of_neutral_bands_is_silent_and_latency_follows_mode() {
        let mut m = Multiband::new();
        neutral(&mut m);
        assert_eq!(m.latency(), 0);
        for b in 0..3 {
            set_plain(&mut m, p::band(b, p::DELTA), 1.0);
        }
        let (l, _) = process_stereo(&mut m, sine(440.0, 0.2), 4800);
        assert!(peak(&l[1000..]) < 1e-6, "{}", peak(&l));
        set_plain(&mut m, p::MODE, mode::LINEAR);
        assert_eq!(m.latency(), LinearPhaseCrossover3::latency_for(SR) as u32);
        assert!(m.latency() > 0);
        process_stereo(&mut m, sine(440.0, 0.2), 512);
        set_plain(&mut m, p::MODE, mode::MINIMUM);
        assert_eq!(m.latency(), 0);
    }

    #[test]
    fn meter_reports_band_levels() {
        let mut m = Multiband::new();
        neutral(&mut m);
        process_stereo(&mut m, sine(80.0, 0.5), 24_000);
        let mut meter = [0.0f32; 3];
        assert_eq!(m.meter(&mut meter), 3);
        assert!(meter[0] > -10.0, "{}", meter[0]);
        assert!(meter[1] < -20.0, "{}", meter[1]);
        assert!(meter[2] < -20.0, "{}", meter[2]);
    }

    #[test]
    fn empty_state_is_restored() {
        let mut m = Multiband::new();
        neutral(&mut m);
        set_plain(&mut m, p::band(2, p::GAIN), -7.0);
        let bytes = m.save_state();
        let mut n = Multiband::new();
        n.load_state(&bytes);
        assert!((n.params.plain(p::band(2, p::GAIN)) + 7.0).abs() < 1e-4);
    }
}
