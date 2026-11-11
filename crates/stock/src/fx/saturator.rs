//! `fx.saturator` — multiband saturation: up to 3 bands split by LR4
//! crossovers, each with its own drive, style and level; ADAA1 shapers run
//! inside a 1×/2×/4× polyphase IIR oversampler (the crossover runs at the
//! oversampled rate too). Global tone tilt, mix (dry aligned to the
//! oversampler's delay), output and optional auto gain.
//!
//! Styles: Tape (tanh with HF pre-/de-emphasis, exact inverse in the linear
//! range), Tube (asymmetric tanh, even harmonics), Clip (hard clip), Fold
//! (sine wavefolder) and Bitcrush (bit depth + sample-rate reduction —
//! aliasing is the point, no ADAA).
//!
//! Meter layout: `[input peak, output peak, band 1 level, band 2 level,
//! band 3 level]` (linear, decaying peaks).

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::crossover::Crossover3;
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::oversample::Oversampler;
use aitrack_dsp::shaper::{Adaa1, Bitcrusher, ShaperKind};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::svf::OnePole;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.saturator";

pub mod p {
    pub const BANDS: u32 = 0;
    pub const XOVER_LOW: u32 = 1;
    pub const XOVER_HIGH: u32 = 2;
    pub const OVERSAMPLING: u32 = 3;
    pub const TONE: u32 = 4;
    pub const MIX: u32 = 5;
    pub const OUTPUT: u32 = 6;
    pub const AUTO_GAIN: u32 = 7;
    /// First per-band id; band `b` uses `BAND0 + b * BAND_STRIDE + offset`.
    pub const BAND0: u32 = 8;
    pub const BAND_STRIDE: u32 = 6;
    pub const DRIVE: u32 = 0;
    pub const STYLE: u32 = 1;
    pub const LEVEL: u32 = 2;
    pub const BIAS: u32 = 3;
    pub const BITS: u32 = 4;
    pub const CRUSH_RATE: u32 = 5;

    /// Id of `offset` (DRIVE, STYLE…) for band `b` (0..3).
    pub const fn band(b: u32, offset: u32) -> u32 {
        BAND0 + b * BAND_STRIDE + offset
    }
}

pub const STYLES: &[&str] = &["Tape", "Tube", "Clip", "Fold", "Bitcrush"];
const BAND_COUNTS: &[&str] = &["1", "2", "3"];
const OS: &[&str] = &["1x", "2x", "4x"];
const OFF_ON: &[&str] = &["Off", "On"];
const DRIVE_NAMES: [&str; 3] = ["Low Drive", "Mid Drive", "High Drive"];
const STYLE_NAMES: [&str; 3] = ["Low Style", "Mid Style", "High Style"];
const LEVEL_NAMES: [&str; 3] = ["Low Level", "Mid Level", "High Level"];
const BIAS_NAMES: [&str; 3] = ["Low Bias", "Mid Bias", "High Bias"];
const BITS_NAMES: [&str; 3] = ["Low Bits", "Mid Bits", "High Bits"];
const RATE_NAMES: [&str; 3] = ["Low Crush Rate", "Mid Crush Rate", "High Crush Rate"];

/// Tape emphasis: +6 dB shelf above ~3 kHz before the curve, undone after.
const TAPE_EMPHASIS: f32 = 1.0;
const TAPE_CORNER: f32 = 3000.0;

pub fn params() -> Vec<ParamInfo> {
    let mut v = vec![
        ParamInfo::new(p::BANDS, "Bands", 1.0, 3.0, 1.0).choices(BAND_COUNTS),
        ParamInfo::new(p::XOVER_LOW, "Crossover Low", 40.0, 2000.0, 200.0).unit("Hz").log().short("X Low"),
        ParamInfo::new(p::XOVER_HIGH, "Crossover High", 500.0, 12_000.0, 2500.0).unit("Hz").log().short("X High"),
        ParamInfo::new(p::OVERSAMPLING, "Oversampling", 0.0, 2.0, 1.0).choices(OS).short("OS"),
        ParamInfo::new(p::TONE, "Tone", -12.0, 12.0, 0.0).unit("dB"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
        ParamInfo::new(p::AUTO_GAIN, "Auto Gain", 0.0, 1.0, 1.0).choices(OFF_ON).short("Auto"),
    ];
    for b in 0..3u32 {
        let i = b as usize;
        v.push(ParamInfo::new(p::band(b, p::DRIVE), DRIVE_NAMES[i], 0.0, 36.0, 6.0).unit("dB").short("Drive"));
        v.push(ParamInfo::new(p::band(b, p::STYLE), STYLE_NAMES[i], 0.0, 4.0, 0.0).choices(STYLES).short("Style"));
        v.push(ParamInfo::new(p::band(b, p::LEVEL), LEVEL_NAMES[i], -24.0, 12.0, 0.0).unit("dB").short("Level"));
        v.push(ParamInfo::new(p::band(b, p::BIAS), BIAS_NAMES[i], 0.0, 100.0, 25.0).unit("%").short("Bias"));
        v.push(ParamInfo::new(p::band(b, p::BITS), BITS_NAMES[i], 1.0, 16.0, 8.0).unit("bit").short("Bits"));
        v.push(ParamInfo::new(p::band(b, p::CRUSH_RATE), RATE_NAMES[i], 500.0, 48_000.0, 12_000.0).unit("Hz").log().short("Rate"));
    }
    v
}

#[derive(Clone, Copy)]
struct Band {
    shaper: Adaa1,
    crush: Bitcrusher,
    pre: OnePole,
    de: OnePole,
}

impl Band {
    fn new() -> Self {
        Self { shaper: Adaa1::new(ShaperKind::Tanh), crush: Bitcrusher::default(), pre: OnePole::default(), de: OnePole::default() }
    }

    fn reset(&mut self) {
        self.shaper.reset();
        self.crush.reset();
        self.pre.reset();
        self.de.reset();
    }
}

/// Per-band settings resolved from the params.
#[derive(Clone, Copy, Default)]
struct BandSet {
    style: usize,
    bias_norm: f32,
    bits: f32,
    rate_ratio: f32,
}

#[derive(Clone, Copy)]
struct Channel {
    os: Oversampler,
    xo: Crossover3,
    bands: [Band; 3],
    tone: OnePole,
}

pub struct Saturator {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    ch: [Channel; 2],
    dry_delay: [DelayLine; 2],
    set: [BandSet; 3],
    drive: [Smoother; 3],
    level: [Smoother; 3],
    wet: Smoother,
    dry: Smoother,
    tone_lo: f32,
    tone_hi: f32,
    factor: usize,
    latency: usize,
    auto_gain: bool,
    peaks: [f32; 5],
    first: bool,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(Saturator::new())
}

impl Default for Saturator {
    fn default() -> Self {
        Self::new()
    }
}

impl Saturator {
    pub fn new() -> Self {
        let params = params();
        let ch = Channel { os: Oversampler::new(2), xo: Crossover3::default(), bands: [Band::new(); 3], tone: OnePole::default() };
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Saturator",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["distortion", "saturation", "multiband"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 48_000.0,
            ch: [ch; 2],
            dry_delay: [DelayLine::new(16), DelayLine::new(16)],
            set: [BandSet::default(); 3],
            drive: [Smoother::new(1.0); 3],
            level: [Smoother::new(1.0); 3],
            wet: Smoother::new(1.0),
            dry: Smoother::new(0.0),
            tone_lo: 1.0,
            tone_hi: 1.0,
            factor: 0,
            latency: 0,
            auto_gain: true,
            peaks: [0.0; 5],
            first: true,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let factor = [1usize, 2, 4][self.params.choice(p::OVERSAMPLING).min(2)];
        if factor != self.factor {
            self.factor = factor;
            for c in &mut self.ch {
                c.os.set_factor(factor);
            }
            self.latency = self.ch[0].os.latency().round() as usize;
        }
        let osr = self.sr * factor as f64;
        let bands = (self.params.plain(p::BANDS).round() as usize).clamp(1, 3);
        let (f1, f2) = (self.params.plain(p::XOVER_LOW), self.params.plain(p::XOVER_HIGH));
        // Corner of the exact de-emphasis inverse (see module docs).
        let w = (std::f64::consts::PI * TAPE_CORNER as f64 / osr).tan() / (1.0 + TAPE_EMPHASIS as f64);
        let de_corner = (w.atan() * osr / std::f64::consts::PI) as f32;
        for c in &mut self.ch {
            c.xo.set(bands, f1, f2.max(f1 * 1.2), osr);
            c.tone.set_freq(1000.0, self.sr);
            for b in &mut c.bands {
                b.pre.set_freq(TAPE_CORNER, osr);
                b.de.set_freq(de_corner, osr);
            }
        }
        for b in 0..3u32 {
            let i = b as usize;
            let style = self.params.choice(p::band(b, p::STYLE)).min(4);
            let bias = self.params.plain(p::band(b, p::BIAS)) / 100.0 * 0.6;
            let kind = match style {
                1 => ShaperKind::Asym,
                2 => ShaperKind::HardClip,
                3 => ShaperKind::Fold,
                _ => ShaperKind::Tanh,
            };
            for c in &mut self.ch {
                c.bands[i].shaper.set_kind(kind);
                c.bands[i].shaper.set_bias(bias as f64);
            }
            let sech2 = 1.0 - bias.tanh().powi(2);
            self.set[i] = BandSet {
                style,
                bias_norm: 1.0 / sech2,
                bits: self.params.plain(p::band(b, p::BITS)),
                rate_ratio: (self.params.plain(p::band(b, p::CRUSH_RATE)) / osr as f32).min(1.0),
            };
            self.drive[i].set_target(db_to_gain(self.params.plain(p::band(b, p::DRIVE))));
            self.level[i].set_target(db_to_gain(self.params.plain(p::band(b, p::LEVEL))));
        }
        let tone = self.params.plain(p::TONE);
        self.tone_lo = db_to_gain(-tone * 0.5);
        self.tone_hi = db_to_gain(tone * 0.5);
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry.set_target((1.0 - mix) * out);
        self.auto_gain = self.params.on(p::AUTO_GAIN);
        if self.first {
            for s in self.drive.iter_mut().chain(self.level.iter_mut()) {
                s.snap_to_target();
            }
            self.wet.snap_to_target();
            self.dry.snap_to_target();
            self.first = false;
        }
    }
}

#[inline]
fn shape(band: &mut Band, set: &BandSet, x: f32, g: f32) -> f32 {
    let u = x * g;
    match set.style {
        0 => {
            let (_, hp) = band.pre.tick(u);
            let e = u + TAPE_EMPHASIS * hp;
            let s = band.shaper.process(e);
            let (lp, hp) = band.de.tick(s);
            lp + hp / (1.0 + TAPE_EMPHASIS)
        }
        1 => band.shaper.process(u) * set.bias_norm,
        2 => band.shaper.process(u),
        // sin(π/2 · 2u/π) = sin(u): unity small-signal gain.
        3 => band.shaper.process(u * std::f32::consts::FRAC_2_PI),
        _ => band.crush.process(u.clamp(-1.0, 1.0), set.bits, set.rate_ratio),
    }
}

impl Segmented for Saturator {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let factor = self.factor;
        let nb = self.ch[0].xo.bands();
        let lat = self.latency;
        let auto = self.auto_gain;
        let (tlo, thi) = (self.tone_lo, self.tone_hi);
        let decay = (-1.0 / (0.3 * self.sr as f32)).exp();
        let set = self.set;
        let Self { ch, dry_delay, drive: drv, level, wet, dry, peaks, .. } = self;
        let mut pk = *peaks;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let mut g = [0.0f32; 3];
            let mut comp = [1.0f32; 3];
            let mut lv = [0.0f32; 3];
            for b in 0..3 {
                g[b] = drv[b].next();
                lv[b] = level[b].next();
                if auto {
                    comp[b] = 1.0 / g[b].sqrt();
                }
            }
            let (w, d) = (wet.next(), dry.next());
            pk[0] = (pk[0] * decay).max(l.abs().max(r.abs()));
            let mut out = [l, r];
            let mut band_pk = [0.0f32; 3];
            for (c, x) in out.iter_mut().enumerate() {
                let chn = &mut ch[c];
                let mut up = [0.0f32; 4];
                chn.os.upsample(*x, &mut up);
                for s in up.iter_mut().take(factor) {
                    let mut split = [0.0f32; 3];
                    chn.xo.split(*s, &mut split);
                    let mut acc = 0.0;
                    for b in 0..nb {
                        let y = shape(&mut chn.bands[b], &set[b], split[b], g[b]) * comp[b] * lv[b];
                        band_pk[b] = band_pk[b].max(y.abs());
                        acc += y;
                    }
                    *s = acc;
                }
                let y = chn.os.downsample(&up);
                let (lo, hi) = chn.tone.tick(y);
                let y = lo * tlo + hi * thi;
                dry_delay[c].push(*x);
                let dry_x = dry_delay[c].read(lat);
                *x = sanitize(dry_x * d + y * w);
            }
            for b in 0..3 {
                pk[2 + b] = (pk[2 + b] * decay).max(band_pk[b]);
            }
            pk[1] = (pk[1] * decay).max(out[0].abs().max(out[1].abs()));
            (out[0], out[1])
        });
        *peaks = pk.map(|v| if v.is_finite() { v } else { 0.0 });
    }
}

impl Processor for Saturator {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        self.sr = sample_rate;
        for s in self.drive.iter_mut().chain(self.level.iter_mut()) {
            s.set_time(20.0, sample_rate);
        }
        self.wet.set_time(20.0, sample_rate);
        self.dry.set_time(20.0, sample_rate);
        self.dry_delay = [DelayLine::new(64), DelayLine::new(64)];
        self.factor = 0;
        self.first = true;
        self.params.mark_dirty();
        self.update();
        self.reset();
    }

    fn reset(&mut self) {
        for c in &mut self.ch {
            c.os.reset();
            c.xo.reset();
            c.tone.reset();
            for b in &mut c.bands {
                b.reset();
            }
        }
        for d in &mut self.dry_delay {
            d.clear();
        }
        self.peaks = [0.0; 5];
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
        self.latency as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(out, 0, &self.peaks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, process_stereo, rms, set_plain, sine};

    /// Power of `x` at `freq` (Goertzel over a whole number of cycles).
    fn power(x: &[f32], freq: f64) -> f64 {
        let w = std::f64::consts::TAU * freq / SR;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s = v as f64 + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - c * s1 * s2) / (x.len() as f64).powi(2)
    }

    fn one_band(s: &mut Saturator, style: f32, drive_db: f32) {
        set_plain(s, p::BANDS, 1.0);
        set_plain(s, p::band(0, p::STYLE), style);
        set_plain(s, p::band(0, p::DRIVE), drive_db);
        set_plain(s, p::AUTO_GAIN, 0.0);
    }

    #[test]
    fn zero_drive_is_nearly_transparent() {
        for style in 0..4 {
            let mut s = Saturator::new();
            one_band(&mut s, style as f32, 0.0);
            let (l, _) = process_stereo(&mut s, sine(1000.0, 0.03), 48_000);
            let x = &l[24_000..48_000];
            let gain = 20.0 * (rms(x) / (0.03 / std::f32::consts::SQRT_2)).log10();
            assert!(gain.abs() < 0.3, "style {style}: {gain} dB");
            let thd = (power(x, 2000.0) + power(x, 3000.0)) / power(x, 1000.0);
            assert!(10.0 * thd.log10() < -50.0, "style {style}: THD {} dB", 10.0 * thd.log10());
        }
    }

    #[test]
    fn drive_adds_harmonics() {
        let mut s = Saturator::new();
        one_band(&mut s, 1.0, 24.0); // tube: even harmonics
        let (l, _) = process_stereo(&mut s, sine(1000.0, 0.3), 48_000);
        let x = &l[24_000..48_000];
        let h2 = 10.0 * (power(x, 2000.0) / power(x, 1000.0)).log10();
        let h3 = 10.0 * (power(x, 3000.0) / power(x, 1000.0)).log10();
        assert!(h2 > -40.0, "2nd harmonic {h2} dB");
        assert!(h3 > -40.0, "3rd harmonic {h3} dB");
    }

    fn alias_ratio_db(os: f32) -> f64 {
        let mut s = Saturator::new();
        one_band(&mut s, 2.0, 24.0); // hard clip
        set_plain(&mut s, p::OVERSAMPLING, os);
        let (l, _) = process_stereo(&mut s, sine(5000.0, 0.5), 96_000);
        let x = &l[48_000..96_000];
        let mut harm = 0.0;
        let mut alias = 0.0;
        for k in 1..24 {
            let f = k as f64 * 1000.0;
            if k % 5 == 0 {
                harm += power(x, f);
            } else {
                alias += power(x, f);
            }
        }
        10.0 * (alias / harm).log10()
    }

    #[test]
    fn oversampling_reduces_aliasing() {
        let a1 = alias_ratio_db(0.0);
        let a4 = alias_ratio_db(2.0);
        assert!(a4 < a1 - 10.0, "1x {a1} dB, 4x {a4} dB");
    }

    #[test]
    fn bands_split_and_latency() {
        let mut s = Saturator::new();
        set_plain(&mut s, p::BANDS, 3.0);
        for b in 0..3 {
            set_plain(&mut s, p::band(b, p::DRIVE), 0.0);
        }
        set_plain(&mut s, p::AUTO_GAIN, 0.0);
        let (l, _) = process_stereo(&mut s, sine(440.0, 0.05), 24_000);
        let gain = 20.0 * (rms(&l[12_000..]) / (0.05 / std::f32::consts::SQRT_2)).log10();
        assert!(gain.abs() < 0.3, "{gain}");
        set_plain(&mut s, p::OVERSAMPLING, 2.0);
        process_stereo(&mut s, sine(440.0, 0.05), 128);
        assert!(s.latency() >= 1 && s.latency() < 10);
        set_plain(&mut s, p::OVERSAMPLING, 0.0);
        process_stereo(&mut s, sine(440.0, 0.05), 128);
        assert_eq!(s.latency(), 0);
    }
}
