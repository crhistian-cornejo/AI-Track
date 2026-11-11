//! `fx.analyzer` — bit-exact pass-through with a spectrum analyser, BS.1770
//! loudness, true peak, peak/RMS and correlation for the UI.
//!
//! Meter layout (`meter()`):
//! * `[0..128)` spectrum in dB, 128 log-spaced bands from 20 Hz to 20 kHz;
//! * `[128]` momentary LUFS, `[129]` short-term LUFS, `[130]` integrated LUFS,
//!   `[131]` loudness range (LU);
//! * `[132]` true peak L (dBTP, held), `[133]` true peak R (dBTP, held);
//! * `[134]` peak L, `[135]` peak R (linear, decaying);
//! * `[136]` RMS L, `[137]` RMS R (linear, 300 ms);
//! * `[138]` correlation (−1..1).

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, save_state, write_meter};
use aitrack_dsp::dynamics::TruePeakMeter;
use aitrack_dsp::loudness::LufsMeter;
use aitrack_dsp::spectrum::SpectrumAnalyzer;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, gain_to_db};

pub const ID: &str = "fx.analyzer";
/// Spectrum bands in the meter output.
pub const BANDS: usize = 128;
/// Total meter length.
pub const METER_LEN: usize = BANDS + 11;

pub mod p {
    pub const FFT_SIZE: u32 = 0;
    pub const FALL: u32 = 1;
    pub const TILT: u32 = 2;
    pub const RESET: u32 = 3;
    pub const SOURCE: u32 = 4;
}

const SIZES: &[&str] = &["2048", "4096", "8192"];
const SIZE_VALUES: [usize; 3] = [2048, 4096, 8192];
const TILTS: &[&str] = &["0 dB/oct", "3 dB/oct", "4.5 dB/oct"];
const TILT_VALUES: [f32; 3] = [0.0, 3.0, 4.5];
const OFF_ON: &[&str] = &["Off", "On"];
const SOURCES: &[&str] = &["Mid", "Left", "Right", "Side"];

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::FFT_SIZE, "FFT Size", 0.0, 2.0, 1.0).choices(SIZES).short("Size"),
        ParamInfo::new(p::FALL, "Fall Rate", 3.0, 120.0, 24.0).unit("dB/s").log().short("Fall"),
        ParamInfo::new(p::TILT, "Tilt", 0.0, 2.0, 0.0).choices(TILTS),
        ParamInfo::new(p::RESET, "Reset Loudness", 0.0, 1.0, 0.0).choices(OFF_ON).short("Reset"),
        ParamInfo::new(p::SOURCE, "Spectrum Source", 0.0, 3.0, 0.0).choices(SOURCES).short("Source"),
    ]
}

pub struct Analyzer {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    spectra: Vec<SpectrumAnalyzer>,
    active: usize,
    lufs: LufsMeter,
    tp: [TruePeakMeter; 2],
    peak: [f32; 2],
    ms: [f32; 2],
    corr: [f32; 3],
    reset_armed: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Analyzer::new())
}

impl Default for Analyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl Analyzer {
    pub fn new() -> Self {
        let params = params();
        let mut a = Self {
            info: PluginInfo {
                id: ID,
                name: "Analyzer",
                vendor: "AI-Track",
                category: Category::Analyzer,
                tags: &["analyzer", "spectrum", "loudness", "meter"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            spectra: Vec::new(),
            active: 1,
            lufs: LufsMeter::new(48_000.0),
            tp: [TruePeakMeter::default(), TruePeakMeter::default()],
            peak: [0.0; 2],
            ms: [0.0; 2],
            corr: [0.0; 3],
            reset_armed: true,
        };
        a.prepare(48_000.0, 512);
        a
    }

    fn update(&mut self) {
        self.active = self.params.choice(p::FFT_SIZE).min(2);
        let fall = self.params.plain(p::FALL);
        let tilt = TILT_VALUES[self.params.choice(p::TILT).min(2)];
        for s in &mut self.spectra {
            s.set_fall_rate(fall);
            s.set_tilt(tilt);
        }
        // Rising edge of the reset switch clears the program readings.
        let reset = self.params.on(p::RESET);
        if reset && self.reset_armed {
            self.lufs.reset();
            for t in &mut self.tp {
                t.reset();
            }
        }
        self.reset_armed = !reset;
    }
}

impl Segmented for Analyzer {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let source = self.params.choice(p::SOURCE);
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let ms_coeff = 1.0 - decay;
        let spectrum = &mut self.spectra[self.active];
        let lufs = &mut self.lufs;
        let [tp_l, tp_r] = &mut self.tp;
        let mut peak = self.peak;
        let mut ms = self.ms;
        let mut corr = self.corr;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let s = match source {
                1 => l,
                2 => r,
                3 => 0.5 * (l - r),
                _ => 0.5 * (l + r),
            };
            spectrum.push_sample(s);
            lufs.push(l, r);
            tp_l.process(l);
            tp_r.process(r);
            peak[0] = (peak[0] * decay).max(l.abs());
            peak[1] = (peak[1] * decay).max(r.abs());
            ms[0] += (l * l - ms[0]) * ms_coeff;
            ms[1] += (r * r - ms[1]) * ms_coeff;
            corr[0] = corr[0] * decay + l * r;
            corr[1] = corr[1] * decay + l * l;
            corr[2] = corr[2] * decay + r * r;
            (l, r)
        });
        let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
        self.peak = peak.map(fin);
        self.ms = ms.map(fin);
        self.corr = corr.map(fin);
    }
}

impl Processor for Analyzer {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        if self.sr != sample_rate || self.spectra.is_empty() {
            self.sr = sample_rate;
            self.spectra = SIZE_VALUES.iter().map(|&n| SpectrumAnalyzer::new(n, BANDS, 20.0, 20_000.0, sample_rate)).collect();
            self.lufs = LufsMeter::new(sample_rate);
        }
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        for s in &mut self.spectra {
            s.reset();
        }
        self.lufs.reset();
        for t in &mut self.tp {
            t.reset();
        }
        self.peak = [0.0; 2];
        self.ms = [0.0; 2];
        self.corr = [0.0; 3];
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
        let n = write_meter(out, 0, self.spectra[self.active].bands());
        let r = self.lufs.reading();
        let den = (self.corr[1] * self.corr[2]).sqrt();
        let corr = if den > 1e-12 { (self.corr[0] / den).clamp(-1.0, 1.0) } else { 0.0 };
        let values = [
            r.momentary,
            r.short_term,
            r.integrated,
            r.range,
            gain_to_db(self.tp[0].max),
            gain_to_db(self.tp[1].max),
            self.peak[0],
            self.peak[1],
            self.ms[0].max(0.0).sqrt(),
            self.ms[1].max(0.0).sqrt(),
            corr,
        ];
        write_meter(out, n.max(BANDS.min(out.len())), &values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{process_stereo, set_plain, sine};

    #[test]
    fn pass_through_is_bit_exact() {
        let mut a = Analyzer::new();
        let input = |i: usize| (((i * 7919) % 1000) as f32 / 500.0 - 1.0, ((i * 104_729) % 977) as f32 / 488.0 - 1.0);
        let (l, r) = process_stereo(&mut a, input, 5000);
        for i in 0..5000 {
            let (x, y) = input(i);
            assert_eq!(l[i].to_bits(), x.to_bits());
            assert_eq!(r[i].to_bits(), y.to_bits());
        }
    }

    #[test]
    fn reads_loudness_and_spectrum() {
        let mut a = Analyzer::new();
        set_plain(&mut a, p::FFT_SIZE, 2.0);
        process_stereo(&mut a, sine(1000.0, 0.1), 48_000 * 5);
        let mut m = vec![0.0; METER_LEN];
        assert_eq!(a.meter(&mut m), METER_LEN);
        assert!((m[BANDS + 2] + 20.0).abs() < 0.2, "integrated {}", m[BANDS + 2]);
        let (i, _) = m[..BANDS].iter().enumerate().max_by(|x, y| x.1.total_cmp(y.1)).unwrap();
        let f = 20.0 * 1000f32.powf(i as f32 / (BANDS - 1) as f32);
        assert!((f / 1000.0 - 1.0).abs() < 0.08, "peak band at {f} Hz");
        // True peak of a -20 dBFS sine ≈ -20 dBTP; correlation of L = R is 1.
        assert!((m[BANDS + 4] + 20.0).abs() < 0.2, "tp {}", m[BANDS + 4]);
        assert!((m[BANDS + 10] - 1.0).abs() < 1e-3);
        assert!((m[BANDS + 8] - 0.1 / std::f32::consts::SQRT_2).abs() < 0.005);
    }

    #[test]
    fn reset_clears_integrated() {
        let mut a = Analyzer::new();
        process_stereo(&mut a, sine(1000.0, 0.1), 48_000 * 2);
        set_plain(&mut a, p::RESET, 1.0);
        // process_stereo prepares (which also resets); drive one block by hand.
        let mut buf = aitrack_dsp::AudioBuffer::new(2, 64);
        let tr = aitrack_dsp::Transport::default();
        let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events: &[], transport: &tr };
        a.process(&mut ctx);
        let mut m = vec![0.0; METER_LEN];
        a.meter(&mut m);
        assert!(m[BANDS + 2] < -100.0);
    }
}
