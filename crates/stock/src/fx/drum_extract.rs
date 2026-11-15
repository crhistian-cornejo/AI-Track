//! `fx.drum_extract` — RX-inspired drum analyzer & extractor pass-through.
//!
//! Passes audio through bit-exact while measuring real-time percussive band
//! activity (Low/Kick, Mid/Snare, High/Hat), transient clarity, and 64-band
//! spectrum. Its parameters (`DE_BLEED`, `DE_REVERB`, `TRANSIENT`,
//! `SENSITIVITY`, `LOOP_BARS`, `ENGINE`, `NORMALIZE`) drive the offline
//! clean one-shot and loop extractor in `aitrack-ai::drum_extract`.
//!
//! Meter layout (`meter()`):
//! * `[0..64)` spectrum in dB, 64 log-spaced bands from 20 Hz to 20 kHz;
//! * `[64]` low-band (kick) percussive energy (0..1);
//! * `[65]` mid-band (snare/clap) percussive energy (0..1);
//! * `[66]` high-band (hat/air) percussive energy (0..1);
//! * `[67]` transient clarity index (0..1);
//! * `[68]` peak L (linear, decaying), `[69]` peak R (linear, decaying).

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, save_state, write_meter};
use aitrack_dsp::spectrum::SpectrumAnalyzer;
use aitrack_dsp::svf::{OnePole, SvfCoeffs, SvfMode, SvfState};
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor};

pub const ID: &str = "fx.drum_extract";
pub const BANDS: usize = 64;
pub const METER_LEN: usize = BANDS + 6;

pub mod p {
    pub const DE_BLEED: u32 = 0;
    pub const DE_REVERB: u32 = 1;
    pub const TRANSIENT: u32 = 2;
    pub const SENSITIVITY: u32 = 3;
    pub const LOOP_BARS: u32 = 4;
    pub const ENGINE: u32 = 5;
    pub const NORMALIZE: u32 = 6;
}

const LOOP_BAR_CHOICES: &[&str] = &["1 Bar", "2 Bars", "4 Bars"];
const ENGINE_CHOICES: &[&str] = &["Auto (Hybrid)", "Pure DSP", "HT-Demucs + RX"];
const NORM_CHOICES: &[&str] = &["-1 dBFS", "-3 dBFS", "-0.3 dBFS", "Off"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::DE_BLEED, "De-Bleed", 0.0, 100.0, 75.0).unit("%").short("De-Bleed"),
        ParamInfo::new(p::DE_REVERB, "De-Reverb", 0.0, 100.0, 45.0).unit("%").short("De-Verb"),
        ParamInfo::new(p::TRANSIENT, "Transient Punch", 0.0, 100.0, 35.0).unit("%").short("Punch"),
        ParamInfo::new(p::SENSITIVITY, "Slice Sensitivity", 0.0, 100.0, 60.0).unit("%").short("Sens"),
        ParamInfo::new(p::LOOP_BARS, "Loop Length", 0.0, 2.0, 1.0).choices(LOOP_BAR_CHOICES).short("Loop"),
        ParamInfo::new(p::ENGINE, "Extraction Engine", 0.0, 2.0, 0.0).choices(ENGINE_CHOICES).short("Engine"),
        ParamInfo::new(p::NORMALIZE, "Peak Normalize", 0.0, 3.0, 0.0).choices(NORM_CHOICES).short("Norm"),
    ]
}

pub struct DrumExtract {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    spectrum: SpectrumAnalyzer,
    lp_sub: SvfState,
    bp_mid: SvfState,
    hp_air: OnePole,
    c_sub: SvfCoeffs,
    c_mid: SvfCoeffs,
    fast_env: [f32; 3],
    slow_env: [f32; 3],
    band_meter: [f32; 3],
    transient_idx: f32,
    peak: [f32; 2],
}

pub fn create() -> Box<dyn Processor> {
    Box::new(DrumExtract::new())
}

impl Default for DrumExtract {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumExtract {
    pub fn new() -> Self {
        let params = params();
        let mut d = Self {
            info: PluginInfo {
                id: ID,
                name: "Drum Extractor",
                vendor: "AI-Track",
                category: Category::Analyzer,
                tags: &["drums", "oneshot", "loop", "extractor", "de-bleed", "analyzer"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            spectrum: SpectrumAnalyzer::new(2048, BANDS, 20.0, 20_000.0, 48_000.0),
            lp_sub: SvfState::default(),
            bp_mid: SvfState::default(),
            hp_air: OnePole::default(),
            c_sub: SvfCoeffs::default(),
            c_mid: SvfCoeffs::default(),
            fast_env: [0.0; 3],
            slow_env: [0.0; 3],
            band_meter: [0.0; 3],
            transient_idx: 0.0,
            peak: [0.0; 2],
        };
        d.prepare(48_000.0, 512);
        d
    }
}

impl Segmented for DrumExtract {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        let _ = self.params.take_dirty();
        let sr = self.sr.max(8_000.0) as f32;
        let fast_a = (-1.0 / (0.008 * sr)).exp();
        let slow_a = (-1.0 / (0.140 * sr)).exp();
        let decay = (-1.0 / (0.250 * sr)).exp();

        let spectrum = &mut self.spectrum;
        let lp_sub = &mut self.lp_sub;
        let bp_mid = &mut self.bp_mid;
        let hp_air = &mut self.hp_air;
        let c_sub = self.c_sub;
        let c_mid = self.c_mid;
        let mut fast = self.fast_env;
        let mut slow = self.slow_env;
        let mut b_meter = self.band_meter;
        let mut peak = self.peak;
        let mut tr_idx = self.transient_idx;

        for_frames(ctx.buffer, start, end, |_, l, r| {
            let mono = 0.5 * (l + r);
            spectrum.push_sample(mono);

            let sub = lp_sub.process(&c_sub, mono);
            let mid = bp_mid.process(&c_mid, mono);
            let (_, air) = hp_air.tick(mono);

            for (idx, sig) in [sub, mid, air].into_iter().enumerate() {
                let a = sig.abs();
                fast[idx] = fast[idx] * fast_a + a * (1.0 - fast_a);
                slow[idx] = slow[idx] * slow_a + a * (1.0 - slow_a);
                let hit = ((fast[idx] - slow[idx] * 0.75).max(0.0) * 3.5).min(1.0);
                b_meter[idx] = (b_meter[idx] * decay).max(hit);
            }
            let tot_fast = fast[0] + fast[1] + fast[2];
            let tot_slow = slow[0] + slow[1] + slow[2];
            let ratio = ((tot_fast - tot_slow * 0.8).max(0.0) / (tot_fast + 1e-4)).clamp(0.0, 1.0);
            tr_idx = (tr_idx * decay).max(ratio);

            peak[0] = (peak[0] * decay).max(l.abs());
            peak[1] = (peak[1] * decay).max(r.abs());
            (l, r)
        });

        let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
        self.fast_env = fast.map(fin);
        self.slow_env = slow.map(fin);
        self.band_meter = b_meter.map(fin);
        self.transient_idx = fin(tr_idx);
        self.peak = peak.map(fin);
    }
}

impl Processor for DrumExtract {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if self.sr != sample_rate {
            self.sr = sample_rate;
            self.spectrum = SpectrumAnalyzer::new(2048, BANDS, 20.0, 20_000.0, sample_rate);
            self.c_sub = SvfCoeffs::new(SvfMode::Lowpass, 160.0, 0.707, 0.0, sample_rate);
            self.c_mid = SvfCoeffs::new(SvfMode::Bandpass, 1100.0, 0.8, 0.0, sample_rate);
            self.hp_air.set_freq(3500.0, sample_rate);
        }
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        self.spectrum.reset();
        self.lp_sub.reset();
        self.bp_mid.reset();
        self.hp_air.reset();
        self.fast_env = [0.0; 3];
        self.slow_env = [0.0; 3];
        self.band_meter = [0.0; 3];
        self.transient_idx = 0.0;
        self.peak = [0.0; 2];
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
        let n = write_meter(out, 0, self.spectrum.bands());
        let tail = [
            self.band_meter[0],
            self.band_meter[1],
            self.band_meter[2],
            self.transient_idx,
            self.peak[0],
            self.peak[1],
        ];
        write_meter(out, n, &tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_dsp::AudioBuffer;

    #[test]
    fn drum_extract_passes_audio_bit_exact_and_meters_transients() {
        let mut proc = DrumExtract::new();
        proc.prepare(48_000.0, 256);
        let mut buf = AudioBuffer::new(2, 256);
        for i in 0..256 {
            let s = if i < 32 { (i as f32 * 0.25).sin() * 0.8 } else { 0.0 };
            buf.channel_mut(0)[i] = s;
            buf.channel_mut(1)[i] = s;
        }
        let orig = buf.channel(0).to_vec();
        let tr = aitrack_dsp::Transport::default();
        let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        proc.process(&mut ctx);
        assert_eq!(buf.channel(0), &orig[..]);
        let mut meter = [0.0f32; METER_LEN];
        let written = proc.meter(&mut meter);
        assert_eq!(written, METER_LEN);
        assert!(meter[68] > 0.1);
    }
}
