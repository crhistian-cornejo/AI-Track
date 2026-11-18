//! `fx.pitchshift` — real-time pitch shifter, −24..+24 semitones plus cents,
//! independent of time.
//!
//! Two read heads sweep a delay line at `1 − ratio` samples per sample, each
//! under a Hann window, half a grain apart so the windows add up to one. When
//! a head wraps (its window is zero) it jumps to the spot of the delay line
//! that best continues the waveform the other head is playing: a normalised
//! cross-correlation over the last ~8 ms inside ±`SEARCH_MS` ("Align"), the
//! idea of waveform-similarity overlap-add. The jump is inaudible because the
//! window is closed, and tonal material keeps its phase instead of beating.
//! Both channels follow heads chosen from the mono sum, so the stereo image
//! holds. A feedback path returns the shifted signal to the input for
//! shimmer-style stacks.
//!
//! The dry signal is delayed by the same latency as the wet path, so the Mix
//! knob never combs. `latency()` reports it for delay compensation: it is
//! that of the longest grain whatever the Grain knob says (a shorter grain
//! reads further back), so automating Grain never moves the delay.
//!
//! The heads read the line through the band-limited kernel of
//! [`aitrack_dsp::resample`]: a head moving `ratio` samples per sample is
//! low-passed at the new Nyquist (cutoff `≈ 1/ratio`), so shifting up does
//! not fold the top of the band back (Hermite left a 13 kHz sine at 22 kHz
//! an octave up at −1.1 dB).
//!
//! Formants are not preserved: material is resampled, as in any
//! delay-line shifter, so large shifts of voices sound like a different singer.
//!
//! Meter layout: `[semitones, phase A, phase B, window A, window B, grain ms, input peak, output peak]`.

use crate::common::{ParamStore, Segmented, drive, for_frames, load_state, sanitize, save_state, write_meter};
use aitrack_dsp::delay::DelayLine;
use aitrack_dsp::resample::{Kernel, Quality};
use aitrack_dsp::smooth::Smoother;
use aitrack_dsp::{Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor, db_to_gain};

pub const ID: &str = "fx.pitchshift";

pub mod p {
    pub const SEMITONES: u32 = 0;
    pub const CENTS: u32 = 1;
    pub const GRAIN: u32 = 2;
    pub const ALIGN: u32 = 3;
    pub const FEEDBACK: u32 = 4;
    pub const MIX: u32 = 5;
    pub const OUTPUT: u32 = 6;
}

const OFF_ON: &[&str] = &["Off", "On"];
/// Half-width of the alignment search around the nominal read position.
const SEARCH_MS: f32 = 8.0;
/// Length of the context compared by the alignment search.
const CONTEXT_MS: f32 = 8.0;
/// Longest grain.
const MAX_GRAIN_MS: f32 = 120.0;

/// Latency in samples for a grain size in ms: the search room plus half a grain.
pub fn latency_samples(grain_ms: f32, sample_rate: f64) -> f32 {
    let ms = 0.001 * sample_rate as f32;
    (SEARCH_MS * ms) as usize as f32 + 4.0 + 0.5 * grain_ms * ms
}

pub fn params() -> Vec<ParamInfo> {
    vec![
        ParamInfo::new(p::SEMITONES, "Semitones", -24.0, 24.0, 0.0).choices(&[]).unit("st").short("Semi"),
        ParamInfo::new(p::CENTS, "Cents", -100.0, 100.0, 0.0).unit("ct").short("Fine"),
        ParamInfo::new(p::GRAIN, "Grain", 10.0, MAX_GRAIN_MS, 40.0).unit("ms").log(),
        ParamInfo::new(p::ALIGN, "Align", 0.0, 1.0, 1.0).choices(OFF_ON),
        ParamInfo::new(p::FEEDBACK, "Feedback", 0.0, 90.0, 0.0).unit("%").short("FB"),
        ParamInfo::new(p::MIX, "Mix", 0.0, 100.0, 100.0).unit("%"),
        ParamInfo::new(p::OUTPUT, "Output", -24.0, 12.0, 0.0).unit("dB").short("Out"),
    ]
}

pub struct PitchShifter {
    info: PluginInfo,
    params: ParamStore,
    sr: f64,
    /// Input (plus feedback) per channel: what the heads read.
    lines: [DelayLine; 2],
    /// Mono sum of `lines`, read by the alignment search.
    mono: DelayLine,
    /// Untouched input, delayed by the latency for the dry path.
    dry: [DelayLine; 2],
    /// Head phase: head A is at `phi`, head B at `phi + 0.5`.
    phi: f32,
    /// Search offset of each head, in samples.
    off: [f32; 2],
    fb_state: [f32; 2],
    semitones: Smoother,
    grain: Smoother,
    bypass: Smoother,
    wet: Smoother,
    dry_gain: Smoother,
    feedback: Smoother,
    first: bool,
    base: f32,
    search: usize,
    context: usize,
    cur_semi: f32,
    win: [f32; 2],
    in_peak: f32,
    out_peak: f32,
}

pub fn create() -> Box<dyn Processor> {
    Box::new(PitchShifter::new())
}

impl Default for PitchShifter {
    fn default() -> Self {
        Self::new()
    }
}

impl PitchShifter {
    pub fn new() -> Self {
        let params = params();
        let mut s = Self {
            info: PluginInfo {
                id: ID,
                name: "Pitch Shifter",
                vendor: "AI-Track",
                category: Category::Effect,
                tags: &["pitch", "transpose", "shifter", "harmony", "shimmer"],
                params: params.clone(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            },
            params: ParamStore::new(&params),
            sr: 0.0,
            lines: [DelayLine::new(16), DelayLine::new(16)],
            mono: DelayLine::new(16),
            dry: [DelayLine::new(16), DelayLine::new(16)],
            phi: 0.0,
            off: [0.0; 2],
            fb_state: [0.0; 2],
            semitones: Smoother::new(0.0),
            grain: Smoother::new(40.0),
            bypass: Smoother::new(1.0),
            wet: Smoother::new(1.0),
            dry_gain: Smoother::new(0.0),
            feedback: Smoother::new(0.0),
            first: true,
            base: 0.0,
            search: 0,
            context: 0,
            cur_semi: 0.0,
            win: [0.0; 2],
            in_peak: 0.0,
            out_peak: 0.0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    fn update(&mut self) {
        let mix = self.params.plain(p::MIX) / 100.0;
        let out = db_to_gain(self.params.plain(p::OUTPUT));
        self.wet.set_target(mix * out);
        self.dry_gain.set_target((1.0 - mix) * out);
        let st = self.params.plain(p::SEMITONES) + self.params.plain(p::CENTS) / 100.0;
        self.semitones.set_target(st);
        self.bypass.set_target(if st.abs() < 0.002 { 1.0 } else { 0.0 });
        self.grain.set_target(self.params.plain(p::GRAIN));
        self.feedback.set_target(self.params.plain(p::FEEDBACK) / 100.0);
        if self.first {
            for s in [&mut self.wet, &mut self.dry_gain, &mut self.semitones, &mut self.bypass, &mut self.grain, &mut self.feedback] {
                s.snap_to_target();
            }
            self.first = false;
        }
    }

    /// Delay between the input and the middle of a grain, in samples.
    fn latency_for(&self, grain_ms: f32) -> f32 {
        latency_samples(grain_ms, self.sr)
    }

    /// Pick the read offset (in samples, within ±`search`) that makes a head
    /// starting at `nominal` continue what the head at `reference` plays.
    fn best_offset(&self, nominal: f32, reference: f32) -> f32 {
        let (search, context) = (self.search as isize, self.context);
        let r0 = reference.round().max(0.0) as usize;
        let mut best = (f32::MIN, 0isize);
        let mut d = -search;
        while d <= search {
            let c0 = (nominal.round() as isize + d).max(1) as usize;
            let (mut dot, mut energy) = (0.0f32, 1e-9f32);
            let mut m = 0;
            while m < context {
                let a = self.mono.read(r0 + m);
                let b = self.mono.read(c0 + m);
                dot += a * b;
                energy += b * b;
                m += 2;
            }
            let score = dot / energy.sqrt();
            if score > best.0 {
                best = (score, d);
            }
            d += 2;
        }
        best.1 as f32
    }

    pub fn grain_ms(&self) -> f32 {
        self.grain.value()
    }
}

impl Segmented for PitchShifter {
    fn event(&mut self, kind: &EventKind) {
        if let EventKind::Param { id, value } = *kind {
            self.params.set(id, value);
        }
    }

    fn render(&mut self, ctx: &mut ProcessContext<'_>, start: usize, end: usize) {
        if self.params.take_dirty() {
            self.update();
        }
        let align = self.params.on(p::ALIGN);
        let ms = 0.001 * self.sr as f32;
        let kernel = Kernel::get(Quality::High);
        let span = self.search as f32;
        // The delay of the longest grain: the one the plugin reports.
        let lat = self.latency_for(MAX_GRAIN_MS);
        let max_grain = MAX_GRAIN_MS * ms;
        for_frames(ctx.buffer, start, end, |_, l, r| {
            let semi = self.semitones.next();
            let grain = (self.grain.next() * ms).max(8.0);
            // A shorter grain reads further back so the middle of the grain stays at `lat`.
            let pad = (0.5 * (max_grain - grain)).max(0.0);
            let byp = self.bypass.next();
            let fb = self.feedback.next();
            self.cur_semi = semi;
            let ratio = (semi * (1.0 / 12.0)).exp2();
            let fc = kernel.cutoff(ratio as f64);
            let input = [l, r];
            self.in_peak = (self.in_peak * 0.9995).max(l.abs().max(r.abs()));
            // Feed the lines: input plus the shifted signal coming back.
            let mut fed = [0.0f32; 2];
            for c in 0..2 {
                fed[c] = input[c] + fb * self.fb_state[c];
                self.lines[c].push(fed[c]);
                self.dry[c].push(input[c]);
            }
            self.mono.push(0.5 * (fed[0] + fed[1]));
            // Advance the heads; a head that wraps picks a new offset.
            let old = [self.phi, self.phi + 0.5];
            let step = (1.0 - ratio) / grain;
            self.phi += step;
            self.phi -= self.phi.floor();
            let ph = [self.phi, (self.phi + 0.5).fract()];
            for k in 0..2 {
                let wrapped = (old[k].fract() - ph[k]).abs() > 0.5;
                if wrapped && byp < 1.0 {
                    if align {
                        let j = 1 - k;
                        let d_ref = self.base + pad + self.off[j] + ph[j] * grain;
                        let nominal = self.base + pad + ph[k] * grain;
                        self.off[k] = self.best_offset(nominal, d_ref).clamp(-span, span);
                    } else {
                        self.off[k] = 0.0;
                    }
                }
            }
            let mut wet = [0.0f32; 2];
            for k in 0..2 {
                let w = 0.5 - 0.5 * (std::f32::consts::TAU * ph[k]).cos();
                self.win[k] = w;
                // `read_delay` keeps the kernel inside the line; only a head
                // at the very end of its (closed) window ever reaches that.
                let d = self.base + pad + self.off[k] + ph[k] * grain;
                for c in 0..2 {
                    wet[c] += w * kernel.read_delay(&self.lines[c], d, fc);
                }
            }
            let (w, dg) = (self.wet.next(), self.dry_gain.next());
            let mut out = [0.0f32; 2];
            for c in 0..2 {
                let delayed = self.dry[c].read_linear(lat);
                let shifted = wet[c] + (self.lines[c].read_linear(lat) - wet[c]) * byp;
                self.fb_state[c] = sanitize(shifted.clamp(-2.0, 2.0));
                out[c] = sanitize(delayed * dg + shifted * w);
            }
            self.out_peak = (self.out_peak * 0.9995).max(out[0].abs().max(out[1].abs()));
            (out[0], out[1])
        });
    }
}

impl Processor for PitchShifter {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, _max_frames: usize) {
        aitrack_dsp::resample::init();
        if self.sr != sample_rate {
            self.sr = sample_rate;
            let ms = 0.001 * sample_rate as f32;
            self.search = (SEARCH_MS * ms) as usize;
            self.context = (CONTEXT_MS * ms) as usize;
            self.base = self.search as f32 + 4.0;
            let n = (self.base + MAX_GRAIN_MS * ms) as usize + self.search + self.context + 16;
            self.lines = [DelayLine::new(n), DelayLine::new(n)];
            self.mono = DelayLine::new(n);
            self.dry = [DelayLine::new(n), DelayLine::new(n)];
        }
        for s in [&mut self.wet, &mut self.dry_gain, &mut self.feedback] {
            s.set_time(20.0, sample_rate);
        }
        self.semitones.set_time(25.0, sample_rate);
        self.bypass.set_time(10.0, sample_rate);
        self.grain.set_time(60.0, sample_rate);
        self.first = true;
        self.params.mark_dirty();
        self.reset();
    }

    fn reset(&mut self) {
        for l in self.lines.iter_mut().chain(self.dry.iter_mut()) {
            l.clear();
        }
        self.mono.clear();
        self.phi = 0.0;
        self.off = [0.0; 2];
        self.fb_state = [0.0; 2];
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
        self.latency_for(MAX_GRAIN_MS).round() as u32
    }

    fn tail(&self) -> u32 {
        // Delay plus the feedback stack decaying to −60 dB.
        let fb = self.params.plain(p::FEEDBACK) / 100.0;
        let passes = if fb > 0.01 { (-3.0 / fb.log10()).clamp(1.0, 60.0) } else { 0.0 };
        (self.latency_for(MAX_GRAIN_MS) * (1.0 + passes)) as u32
    }

    fn save_state(&self) -> Vec<u8> {
        save_state(&self.params, serde_json::Value::Null)
    }

    fn load_state(&mut self, data: &[u8]) {
        load_state(&mut self.params, data);
    }

    fn meter(&self, out: &mut [f32]) -> usize {
        write_meter(
            out,
            0,
            &[self.cur_semi, self.phi, (self.phi + 0.5).fract(), self.win[0], self.win[1], self.grain.value(), self.in_peak.min(4.0), self.out_peak.min(4.0)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{SR, process_stereo, rms, set_plain, sine};

    /// Level of `freq` in `x` (Goertzel, Hann-windowed).
    fn level_at(x: &[f32], freq: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        let n = x.len() as f64;
        for (i, &v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos();
            let a = std::f64::consts::TAU * freq as f64 * i as f64 / SR;
            re += v as f64 * w * a.cos();
            im += v as f64 * w * a.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / (n * 0.5)) as f32
    }

    /// Frequency with the most energy between `lo` and `hi`, 2 Hz resolution.
    fn peak_freq(x: &[f32], lo: f32, hi: f32) -> f32 {
        let mut best = (0.0f32, lo);
        let mut f = lo;
        while f <= hi {
            let v = level_at(x, f);
            if v > best.0 {
                best = (v, f);
            }
            f += 2.0;
        }
        best.1
    }

    #[test]
    fn octave_up_moves_the_energy_to_880() {
        let mut s = PitchShifter::new();
        set_plain(&mut s, p::SEMITONES, 12.0);
        let (l, _) = process_stereo(&mut s, sine(440.0, 0.5), 48_000);
        let tail = &l[16_000..];
        let (at_440, at_880) = (level_at(tail, 440.0), level_at(tail, 880.0));
        assert!(at_880 > 0.3, "880 Hz level {at_880}");
        assert!(at_880 > 8.0 * at_440, "440 {at_440} 880 {at_880}");
        assert!((peak_freq(tail, 300.0, 1200.0) - 880.0).abs() < 6.0);
    }

    #[test]
    fn fifth_down_and_fine_tuning() {
        let mut s = PitchShifter::new();
        set_plain(&mut s, p::SEMITONES, -7.0);
        let (l, _) = process_stereo(&mut s, sine(440.0, 0.5), 48_000);
        let want = 440.0 * 2f32.powf(-7.0 / 12.0);
        assert!((peak_freq(&l[16_000..], 200.0, 600.0) - want).abs() < 6.0, "{}", peak_freq(&l[16_000..], 200.0, 600.0));
        // +50 cents.
        let mut s = PitchShifter::new();
        set_plain(&mut s, p::CENTS, 50.0);
        let (l, _) = process_stereo(&mut s, sine(440.0, 0.5), 48_000);
        let want = 440.0 * 2f32.powf(0.5 / 12.0);
        assert!((peak_freq(&l[16_000..], 400.0, 500.0) - want).abs() < 4.0);
    }

    #[test]
    fn align_keeps_a_tone_steadier_than_plain_crossfades() {
        // A fifth up: with the waveform search the level of the shifted sine
        // varies less than with free-running heads.
        let ripple = |align: f32| {
            let mut s = PitchShifter::new();
            set_plain(&mut s, p::SEMITONES, 7.0);
            set_plain(&mut s, p::ALIGN, align);
            let (l, _) = process_stereo(&mut s, sine(330.0, 0.5), 48_000);
            let levels: Vec<f32> = l[12_000..].chunks(240).map(rms).collect();
            let (lo, hi) = levels.iter().fold((f32::MAX, 0.0f32), |(a, b), &v| (a.min(v), b.max(v)));
            hi / lo.max(1e-6)
        };
        let (on, off) = (ripple(1.0), ripple(0.0));
        assert!(on < off, "aligned {on} free {off}");
        assert!(on < 1.6, "aligned ripple {on}");
    }

    #[test]
    fn zero_shift_is_a_clean_delay_and_latency_matches() {
        let mut s = PitchShifter::new();
        let lat = s.latency() as usize;
        assert!(lat > 0 && lat < 6000, "latency {lat}");
        let (l, _) = process_stereo(&mut s, |i| if i == 2000 { (1.0, 1.0) } else { (0.0, 0.0) }, 12_000);
        let at = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!((at as i64 - 2000 - lat as i64).abs() <= 2, "impulse at {at}, latency {lat}");
        assert!(l[at] > 0.95);
    }

    #[test]
    fn latency_does_not_follow_the_grain_knob() {
        let mut reported = Vec::new();
        for grain in [10.0, 40.0, 120.0] {
            let mut s = PitchShifter::new();
            set_plain(&mut s, p::GRAIN, grain);
            let lat = s.latency() as usize;
            reported.push(lat);
            let (l, _) = process_stereo(&mut s, |i| if i == 2000 { (1.0, 1.0) } else { (0.0, 0.0) }, 16_000);
            let at = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
            assert!((at as i64 - 2000 - lat as i64).abs() <= 2, "grain {grain}: impulse at {at}, latency {lat}");
        }
        assert!(reported.windows(2).all(|w| w[0] == w[1]), "{reported:?}");
        // Shifted, the wet path is as late as the dry one: a short grain still centres on the latency.
        let mut s = PitchShifter::new();
        set_plain(&mut s, p::GRAIN, 10.0);
        set_plain(&mut s, p::SEMITONES, 12.0);
        assert_eq!(s.latency() as usize, reported[0]);
    }

    #[test]
    fn mix_zero_is_the_delayed_input() {
        let mut s = PitchShifter::new();
        set_plain(&mut s, p::SEMITONES, 5.0);
        set_plain(&mut s, p::MIX, 0.0);
        let lat = s.latency() as usize;
        let (l, _) = process_stereo(&mut s, sine(440.0, 0.5), 12_000);
        let (want, _) = sine(440.0, 0.5)(9000 - lat);
        assert!((l[9000] - want).abs() < 1e-3, "{} vs {want}", l[9000]);
    }

    #[test]
    fn feedback_stays_bounded() {
        let mut s = PitchShifter::new();
        set_plain(&mut s, p::SEMITONES, 12.0);
        set_plain(&mut s, p::FEEDBACK, 90.0);
        let (l, r) = process_stereo(&mut s, |i| if i % 997 == 0 { (1.0, 1.0) } else { (0.0, 0.0) }, 96_000);
        assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 30.0));
    }

    #[test]
    fn stereo_image_is_kept() {
        let mut s = PitchShifter::new();
        set_plain(&mut s, p::SEMITONES, 4.0);
        let (l, r) = process_stereo(&mut s, |i| ((i as f32 * 0.05).sin() * 0.5, 0.0), 24_000);
        assert!(rms(&l[8000..]) > 0.05);
        assert!(rms(&r) < 1e-6);
    }

    /// Report 24.01 §5.1: +12 st on a 13 kHz sine left 22 kHz at −1.1 dB.
    /// Reading the line faster than the rate now goes through a kernel whose
    /// cutoff follows the ratio, so the octave above Nyquist vanishes.
    #[test]
    fn octave_up_does_not_fold_back() {
        let run = |freq: f32| {
            let mut s = PitchShifter::new();
            set_plain(&mut s, p::SEMITONES, 12.0);
            let (l, _) = process_stereo(&mut s, sine(freq, 0.5), 96_000);
            l[48_000..80_000].to_vec()
        };
        let reference = run(4_000.0);
        let alias = crate::test_util::rel_db(&run(13_000.0), &reference);
        let pass = 20.0 * (rms(&reference) / (0.5 / 2f32.sqrt())).log10();
        eprintln!("pitchshift +12 st: 13 kHz residual {alias:.1} dB, 4 kHz -> 8 kHz gain {pass:+.2} dB");
        assert!(alias < -60.0, "13 kHz an octave up leaves {alias:.1} dB");
        assert!(pass.abs() < 0.5, "passband gain {pass:+.2} dB");
    }

}
