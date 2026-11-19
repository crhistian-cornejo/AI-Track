//! Ableton-style time-stretching and pitch-shifting DSP.
//!
//! Supports all six modes:
//! - [`WarpAlgorithm::Beats`]: transient onset detection and transient-preserving slice playback
//! - [`WarpAlgorithm::Tones`]: stereo-linked pitch-period NCC WSOLA
//! - [`WarpAlgorithm::Texture`]: stochastic multi-grain granular overlap-add
//! - [`WarpAlgorithm::Complex`]: Laroche–Dolson peak-phase-locked STFT phase vocoder with transient reset
//! - [`WarpAlgorithm::ComplexPro`]: phase-locked STFT phase vocoder with real-cepstrum formant envelope preservation
//! - [`WarpAlgorithm::Repitch`]: speed-linked band-limited resampling ([`crate::resample`])
//!
//! Complex and Complex Pro render with Signalsmith Stretch (MIT) when the
//! `signalsmith` feature is on (the default); [`Stretcher::Native`] keeps our
//! own phase vocoder for A/B and for clips too short for its window. Beats,
//! Tones and Texture are always ours. Every transposition is resampled through the
//! band-limited kernel, whose cutoff follows the ratio.
//!
//! Offline warping ([`warp_audio`]) produces pre-rendered planar audio off the
//! real-time audio thread (cached by the engine). For dynamic live tempo ramps
//! or uncached tempo changes on the audio thread, [`sample_warped_beats`] and
//! [`sample_warped_grain`] evaluate statelessly with zero heap allocations.

use crate::resample::Quality;
use realfft::RealFftPlanner;
use realfft::num_complex::Complex;
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WarpAlgorithm {
    Beats,
    Tones,
    Texture,
    Complex,
    ComplexPro,
    Repitch,
}

#[inline]
fn cubic_hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

#[inline]
pub fn read_cubic(slice: &[f32], pos: f64) -> f32 {
    let len = slice.len();
    if len == 0 || !pos.is_finite() {
        return 0.0;
    }
    if pos < -1.0 || pos >= len as f64 + 1.0 {
        return 0.0;
    }
    let idx = pos.floor() as isize;
    let t = (pos - idx as f64) as f32;
    let at = |i: isize| -> f32 {
        if i < 0 || i as usize >= len { 0.0 } else { slice[i as usize] }
    };
    cubic_hermite(at(idx - 1), at(idx), at(idx + 1), at(idx + 2), t)
}

/// Read `channels` to `out_len` frames at `in_len / out_len` frames per
/// frame, through the band-limited [`resample`](crate::resample) kernel:
/// the cutoff follows the ratio, so transposing up does not fold the top
/// of the band back (Hermite used to leave a 13 kHz sine at full level at
/// 22 kHz an octave up).
fn resample_channels(channels: &[&[f32]], out_len: usize) -> Vec<Vec<f32>> {
    crate::resample::resample(channels, out_len, Quality::Offline)
}

fn periodic_hann(size: usize) -> Vec<f32> {
    (0..size).map(|i| 0.5 - 0.5 * (TAU * i as f32 / size as f32).cos()).collect()
}

/// Detect percussive/note onset transient frame indices across all channels.
pub fn detect_transients(channels: &[&[f32]], sample_rate: u32) -> Vec<usize> {
    detect_transients_with(channels, sample_rate, 0.5)
}

/// [`detect_transients`] with a sensitivity 0..=1: 0.5 is the default
/// threshold, higher finds softer onsets (more of them), lower only the
/// clear hits. The first transient is always frame 0.
pub fn detect_transients_with(channels: &[&[f32]], sample_rate: u32, sensitivity: f32) -> Vec<usize> {
    // Threshold scale: 2 at sensitivity 0, 1 at 0.5, 0.5 at 1.
    let k = 2f32.powf(1.0 - 2.0 * sensitivity.clamp(0.0, 1.0));
    let len = channels.first().map_or(0, |c| c.len());
    if len == 0 || sample_rate == 0 {
        return Vec::new();
    }
    let sr = sample_rate as usize;
    let hop = (sr / 200).max(32); // 5 ms hop
    let win = (hop * 2).max(64);
    let n_frames = len.saturating_sub(win) / hop + 1;
    let mut flux = Vec::with_capacity(n_frames);
    let mut prev_rms = 0.0f32;
    let mut prev_hf = 0.0f32;
    let n_ch = channels.len().max(1) as f32;

    for f in 0..n_frames {
        let start = f * hop;
        let end = (start + win).min(len);
        let mut e = 0.0f32;
        let mut hf = 0.0f32;
        for i in start..end {
            let mut s = 0.0f32;
            let mut prev_s = 0.0f32;
            for ch in channels {
                s += ch[i];
                prev_s += if i > 0 { ch[i - 1] } else { ch[i] };
            }
            s /= n_ch;
            prev_s /= n_ch;
            e += s * s;
            let d = s - prev_s;
            hf += d * d;
        }
        let inv = 1.0 / (end - start).max(1) as f32;
        let rms = (e * inv).sqrt();
        let hf_rms = (hf * inv).sqrt();
        let df = (rms - prev_rms).max(0.0) + 0.6 * (hf_rms - prev_hf).max(0.0);
        flux.push(df);
        prev_rms = rms;
        prev_hf = hf_rms;
    }

    let refractory = ((sr as f32 * 0.045) / hop as f32).ceil() as usize; // 45 ms
    let back_snap = sr / 500; // snap 2 ms earlier to preserve attack crispness
    let mut transients = vec![0usize];
    let mut last_idx = 0usize;
    let floor = 0.008f32 * k;

    for i in 1..flux.len() {
        let cur = flux[i];
        let is_peak = cur >= flux[i - 1] && (i + 1 >= flux.len() || cur > flux[i + 1]);
        if !is_peak || cur < floor {
            continue;
        }
        let lo = i.saturating_sub(8);
        let hi = (i + 9).min(flux.len());
        let mut window_vals: Vec<f32> = flux[lo..hi].to_vec();
        window_vals.sort_by(f32::total_cmp);
        let median = window_vals[window_vals.len() / 2];
        if cur > median * 2.2 * k + floor && i.saturating_sub(last_idx) >= refractory.max(1) {
            let frame = (i * hop).saturating_sub(back_snap);
            if frame > *transients.last().unwrap_or(&0) + back_snap {
                transients.push(frame);
                last_idx = i;
            }
        }
    }
    transients
}

// ───────────────────────────── Beats (Transient Slices) ─────────────────────────────

fn stretch_beats_slices(channels: &[&[f32]], sample_rate: u32, time_scale: f64, transients: &[usize]) -> Vec<Vec<f32>> {
    let in_len = channels.first().map_or(0, |c| c.len());
    if in_len == 0 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    let out_len = ((in_len as f64 * time_scale).round() as usize).max(1);
    if (time_scale - 1.0).abs() < 1e-4 {
        return channels.iter().map(|c| c.to_vec()).collect();
    }
    let mut cuts: Vec<usize> = transients.iter().copied().filter(|&t| t < in_len).collect();
    if cuts.first().copied() != Some(0) {
        cuts.insert(0, 0);
    }
    cuts.dedup();
    cuts.push(in_len);

    let xfade = ((sample_rate as usize) / 160).clamp(16, 512); // ~6 ms crossfade
    let attack_guard = ((sample_rate as usize) / 25).max(128); // protect first 40 ms attack from tail loop
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| vec![0.0; out_len]).collect();
    let mut prev_tail_end = 0usize;

    for w in cuts.windows(2) {
        let s0 = w[0];
        let s1 = w[1];
        let slice_len = s1.saturating_sub(s0);
        if slice_len == 0 {
            continue;
        }
        let d0 = ((s0 as f64 * time_scale).round() as usize).min(out_len);
        let d1 = ((s1 as f64 * time_scale).round() as usize).min(out_len);
        let target_len = d1.saturating_sub(d0);
        if target_len == 0 {
            continue;
        }

        // Crossfade slice boundary if overlapping previous truncation.
        let fade_in = if d0 > 0 && prev_tail_end > 0 { xfade.min(target_len / 2).min(slice_len / 2) } else { 0 };
        let loop_start = s0 + attack_guard.min(slice_len * 2 / 3);
        let tail_len = s1.saturating_sub(loop_start).max(1);

        for (c_idx, ch) in channels.iter().enumerate() {
            let dst = &mut out[c_idx];
            for j in 0..target_len {
                let val = if j < slice_len {
                    ch[s0 + j]
                } else {
                    // Ping-pong inside slice decay tail with micro-crossfade at turnaround.
                    let extra = j - slice_len;
                    let period = (tail_len * 2).max(2);
                    let p = extra % period;
                    let src_idx = if p < tail_len { s1.saturating_sub(1 + p) } else { loop_start + (p - tail_len) };
                    // Gentle exponential decay on extended tail so long stretches stay musical.
                    let decay = (-2.2 * (extra as f32 / (sample_rate.max(1) as f32 * 0.5))).exp().max(0.05);
                    ch[src_idx.clamp(s0, s1 - 1)] * decay
                };
                let w_in = if j < fade_in && fade_in > 0 {
                    0.5 - 0.5 * (PI * j as f32 / fade_in as f32).cos()
                } else {
                    1.0
                };
                dst[d0 + j] = if j < fade_in { dst[d0 + j] * (1.0 - w_in) + val * w_in } else { val };
            }
        }
        // Pre-fade out at the end of a truncated slice so the next transient joins cleanly.
        if target_len < slice_len && d1 < out_len {
            let fade_out = xfade.min(target_len / 2);
            for dst in &mut out {
                for j in 0..fade_out {
                    let g = 0.5 + 0.5 * (PI * j as f32 / fade_out as f32).cos();
                    dst[d1 - fade_out + j] *= g;
                }
            }
            prev_tail_end = d1;
        } else {
            prev_tail_end = 0;
        }
    }
    out
}

// ───────────────────────────── WSOLA (Tones / Beats Pitch) ─────────────────────────────

fn wsola_stretch(channels: &[&[f32]], time_scale: f64, win_size: usize, max_search: usize) -> Vec<Vec<f32>> {
    let in_len = channels.first().map_or(0, |c| c.len());
    if in_len == 0 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    let out_len = ((in_len as f64 * time_scale).round() as usize).max(1);
    if in_len <= win_size || (time_scale - 1.0).abs() < 1e-4 {
        return resample_channels(channels, out_len);
    }

    let hop_syn = win_size / 4;
    let window = periodic_hann(win_size);
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| vec![0.0; out_len]).collect();
    let mut norm = vec![0.0f32; out_len];

    // Write first grain unshifted at 0.
    for i in 0..win_size.min(out_len).min(in_len) {
        let w = window[i];
        norm[i] += w;
        for (c, ch) in channels.iter().enumerate() {
            out[c][i] += ch[i] * w;
        }
    }

    let mut out_pos = hop_syn;
    let mut prev_nat = hop_syn as isize;

    while out_pos < out_len {
        let target_src = (out_pos as f64 * (in_len as f64 / out_len as f64)).round() as isize;
        let lo = (target_src - max_search as isize).max(0);
        let hi = (target_src + max_search as isize).min(in_len.saturating_sub(win_size) as isize);

        let mut best_pos = target_src.clamp(0, in_len.saturating_sub(win_size) as isize) as usize;
        if hi >= lo && prev_nat >= 0 && (prev_nat as usize + hop_syn) <= in_len {
            let ref_start = prev_nat as usize;
            let corr_len = (win_size / 2).min(in_len.saturating_sub(ref_start));
            let mut best_score = f32::NEG_INFINITY;
            // Coarse step 4 then fine ±4 keeps NCC fast and deterministic.
            let mut cand = lo;
            while cand <= hi {
                let c = cand as usize;
                let mut num = 0.0f32;
                let mut den = 1e-9f32;
                for ch in channels {
                    let mut i = 0;
                    while i < corr_len {
                        let a = ch[ref_start + i];
                        let b = ch[c + i];
                        num += a * b;
                        den += b * b;
                        i += 2;
                    }
                }
                let score = num / den.sqrt();
                if score > best_score {
                    best_score = score;
                    best_pos = c;
                }
                cand += 4;
            }
            let fine_lo = (best_pos as isize - 3).max(lo);
            let fine_hi = (best_pos as isize + 3).min(hi);
            for cand in fine_lo..=fine_hi {
                let c = cand as usize;
                let mut num = 0.0f32;
                let mut den = 1e-9f32;
                for ch in channels {
                    for i in 0..corr_len {
                        let a = ch[ref_start + i];
                        let b = ch[c + i];
                        num += a * b;
                        den += b * b;
                    }
                }
                let score = num / den.sqrt();
                if score > best_score {
                    best_score = score;
                    best_pos = c;
                }
            }
        }

        let write_len = win_size.min(out_len - out_pos).min(in_len.saturating_sub(best_pos));
        for i in 0..write_len {
            let w = window[i];
            norm[out_pos + i] += w;
            for (c, ch) in channels.iter().enumerate() {
                out[c][out_pos + i] += ch[best_pos + i] * w;
            }
        }
        // WSOLA natural continuation in the waveform: `hop_syn` after the
        // previous grain's source offset so the overlap-add region stays in phase.
        prev_nat = best_pos as isize + hop_syn as isize;
        out_pos += hop_syn;
    }

    for (c, dst) in out.iter_mut().enumerate() {
        for (i, v) in dst.iter_mut().enumerate() {
            if norm[i] > 1e-5 {
                *v /= norm[i];
            } else if i < channels[c].len() {
                *v = channels[c][i];
            }
        }
    }
    out
}

// ───────────────────────────── Texture (Granular Scatter) ─────────────────────────────

#[inline]
fn splitmix_signed(mut x: u64) -> f32 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    let u = ((x >> 40) as f32) / 16_777_215.0;
    u * 2.0 - 1.0
}

fn granular_texture_stretch(channels: &[&[f32]], sample_rate: u32, time_scale: f64) -> Vec<Vec<f32>> {
    let in_len = channels.first().map_or(0, |c| c.len());
    if in_len == 0 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    let out_len = ((in_len as f64 * time_scale).round() as usize).max(1);
    let win_size = ((sample_rate as usize * 64) / 1000).clamp(512, 4096);
    if in_len <= win_size / 2 {
        return resample_channels(channels, out_len);
    }
    let hop_syn = (win_size / 6).max(64);
    let jitter = ((sample_rate as usize * 18) / 1000).min(in_len / 2);
    let window = periodic_hann(win_size);

    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| vec![0.0; out_len]).collect();
    let mut norm: Vec<Vec<f32>> = channels.iter().map(|_| vec![0.0; out_len]).collect();

    let mut grain = 0u64;
    let mut out_pos = 0usize;
    let max_start = in_len.saturating_sub(win_size);
    while out_pos < out_len {
        let base_src = ((out_pos as f64) * (in_len as f64 / out_len as f64)).round() as isize;
        for (c, ch) in channels.iter().enumerate() {
            // Slight stereo grain decorrelation keeps pads wide & smooth.
            let j = (splitmix_signed(grain ^ ((c as u64 + 1) * 0xA24B_AED4_963E_E407)) * jitter as f32).round() as isize;
            let src_start = (base_src + j).clamp(0, max_start as isize) as usize;
            let write_len = win_size.min(out_len - out_pos).min(in_len - src_start);
            for i in 0..write_len {
                let w = window[i];
                out[c][out_pos + i] += ch[src_start + i] * w;
                norm[c][out_pos + i] += w;
            }
        }
        grain += 1;
        out_pos += hop_syn;
    }

    for (c, dst) in out.iter_mut().enumerate() {
        for (i, v) in dst.iter_mut().enumerate() {
            if norm[c][i] > 1e-5 {
                *v /= norm[c][i];
            }
        }
    }
    out
}

// ───────────────────────────── Complex & Complex Pro (Phase Vocoder) ─────────────────────────────

#[inline]
fn wrap_phase(p: f32) -> f32 {
    (p + PI).rem_euclid(TAU) - PI
}

struct CepstrumScratch<'a> {
    fft: &'a dyn realfft::RealToComplex<f32>,
    ifft: &'a dyn realfft::ComplexToReal<f32>,
    log_buf: Vec<f32>,
    cep: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    ifft_scratch: Vec<Complex<f32>>,
    orig_env: Vec<f32>,
    shifted_env: Vec<f32>,
}

impl<'a> CepstrumScratch<'a> {
    fn new(fft: &'a dyn realfft::RealToComplex<f32>, ifft: &'a dyn realfft::ComplexToReal<f32>, fft_size: usize) -> Self {
        let n_bins = fft_size / 2 + 1;
        Self {
            fft,
            ifft,
            log_buf: vec![0.0; fft_size],
            cep: vec![Complex::default(); n_bins],
            fft_scratch: fft.make_scratch_vec(),
            ifft_scratch: ifft.make_scratch_vec(),
            orig_env: vec![1.0; n_bins],
            shifted_env: vec![1.0; n_bins],
        }
    }

    /// Extract a smooth spectral magnitude envelope via real cepstrum liftering.
    fn compute_envelope(&mut self, mag: &[f32], lifter: usize) {
        let fft_size = self.log_buf.len();
        let bins = mag.len();
        for (k, &m) in mag.iter().enumerate() {
            self.cep[k] = Complex::new((m.max(1e-7)).ln(), 0.0);
        }
        let _ = self.ifft.process_with_scratch(&mut self.cep, &mut self.log_buf, &mut self.ifft_scratch);
        let scale = 1.0 / fft_size as f32;
        let keep = lifter.min(fft_size / 2);
        for (q, v) in self.log_buf.iter_mut().enumerate() {
            let dist = q.min(fft_size - q);
            if dist >= keep {
                *v = 0.0;
            } else {
                let w = if dist > 0 { 0.5 + 0.5 * (PI * dist as f32 / keep as f32).cos() } else { 1.0 };
                *v *= scale * w;
            }
        }
        let _ = self.fft.process_with_scratch(&mut self.log_buf, &mut self.cep, &mut self.fft_scratch);
        for k in 0..bins {
            self.orig_env[k] = self.cep[k].re.clamp(-12.0, 12.0).exp();
        }
    }
}

fn phase_vocoder(channels: &[&[f32]], time_scale: f64, pitch_ratio: f64, formant_preserve: bool) -> Vec<Vec<f32>> {
    let in_len = channels.first().map_or(0, |c| c.len());
    if in_len == 0 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    let out_len = ((in_len as f64 * time_scale).round() as usize).max(1);
    let fft_size = 2048usize;
    if in_len < fft_size {
        return wsola_stretch(channels, time_scale, (in_len / 2).max(64).next_power_of_two().min(512), 128);
    }

    // Combined time-stretch factor before resampling by pitch_ratio.
    let stretch = (time_scale * pitch_ratio).max(0.05);
    let pv_out_len = ((in_len as f64 * stretch).round() as usize).max(fft_size);
    let hop_syn = 512usize;
    let n_bins = fft_size / 2 + 1;
    let n_ch = channels.len();

    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let ifft = planner.plan_fft_inverse(fft_size);
    let window = periodic_hann(fft_size);
    let inv_fft = 1.0 / fft_size as f32;

    let mut out: Vec<Vec<f32>> = vec![vec![0.0; pv_out_len]; n_ch];
    let mut norm = vec![0.0f32; pv_out_len];

    let mut time_buf = vec![0.0f32; fft_size];
    let mut spec: Vec<Vec<Complex<f32>>> = vec![vec![Complex::default(); n_bins]; n_ch];
    let mut prev_phase: Vec<Vec<f32>> = vec![vec![0.0; n_bins]; n_ch];
    let mut syn_phase: Vec<Vec<f32>> = vec![vec![0.0; n_bins]; n_ch];
    let mut mag: Vec<Vec<f32>> = vec![vec![0.0; n_bins]; n_ch];
    let mut phase: Vec<Vec<f32>> = vec![vec![0.0; n_bins]; n_ch];
    let mut scratch = fft.make_scratch_vec();
    let mut ifft_scratch = ifft.make_scratch_vec();
    let mut combined_mag = vec![0.0f32; n_bins];
    let mut prev_energy = 0.0f32;

    // Cepstral buffers for Complex Pro formant preservation.
    let mut cep_ws = CepstrumScratch::new(fft.as_ref(), ifft.as_ref(), fft_size);

    let n_hops = (pv_out_len.saturating_sub(fft_size)) / hop_syn + 1;
    let mut prev_ana_pos = 0.0f64;

    for h in 0..n_hops {
        let out_pos = h * hop_syn;
        let ana_pos = ((out_pos as f64) / stretch).clamp(0.0, (in_len - fft_size) as f64);
        let hop_ana = if h == 0 { hop_syn as f32 } else { (ana_pos - prev_ana_pos) as f32 };
        prev_ana_pos = ana_pos;

        // Forward STFT per channel.
        let mut frame_energy = 0.0f32;
        combined_mag.fill(0.0);
        for c in 0..n_ch {
            for i in 0..fft_size {
                time_buf[i] = read_cubic(channels[c], ana_pos + i as f64) * window[i];
            }
            let _ = fft.process_with_scratch(&mut time_buf, &mut spec[c], &mut scratch);
            spec[c][0].im = 0.0;
            spec[c][n_bins - 1].im = 0.0;
            for k in 0..n_bins {
                let re = spec[c][k].re;
                let im = spec[c][k].im;
                let m = (re * re + im * im).sqrt();
                mag[c][k] = m;
                phase[c][k] = im.atan2(re);
                combined_mag[k] += m;
                frame_energy += m * m;
            }
        }

        let is_transient = h == 0 || (frame_energy > prev_energy * 2.8 && frame_energy > 0.5);
        prev_energy = frame_energy * 0.65 + prev_energy * 0.35;

        if is_transient {
            for c in 0..n_ch {
                syn_phase[c].copy_from_slice(&phase[c]);
                prev_phase[c].copy_from_slice(&phase[c]);
            }
        } else {
            // Stereo-linked spectral peaks + region-of-influence vertical phase locking.
            let mut peaks: Vec<usize> = Vec::new();
            for k in 1..n_bins - 1 {
                if combined_mag[k] > combined_mag[k - 1] && combined_mag[k] >= combined_mag[k + 1] && combined_mag[k] > 1e-6 {
                    peaks.push(k);
                }
            }
            for c in 0..n_ch {
                if peaks.is_empty() {
                    for k in 0..n_bins {
                        let omega = TAU * k as f32 / fft_size as f32;
                        let dp = wrap_phase(phase[c][k] - prev_phase[c][k] - omega * hop_ana);
                        syn_phase[c][k] = wrap_phase(syn_phase[c][k] + (omega + dp / hop_ana.max(1.0)) * hop_syn as f32);
                    }
                } else {
                    // Advance synthesis phase along each spectral peak, then lock surrounding bins.
                    let mut peak_syn = vec![0.0f32; peaks.len()];
                    for (pi, &pk) in peaks.iter().enumerate() {
                        let omega = TAU * pk as f32 / fft_size as f32;
                        let dp = wrap_phase(phase[c][pk] - prev_phase[c][pk] - omega * hop_ana);
                        peak_syn[pi] = wrap_phase(syn_phase[c][pk] + (omega + dp / hop_ana.max(1.0)) * hop_syn as f32);
                    }
                    let mut cur_pi = 0usize;
                    for k in 0..n_bins {
                        while cur_pi + 1 < peaks.len() && (k as isize - peaks[cur_pi + 1] as isize).abs() < (k as isize - peaks[cur_pi] as isize).abs() {
                            cur_pi += 1;
                        }
                        let pk = peaks[cur_pi];
                        let rot = peak_syn[cur_pi] - phase[c][pk];
                        syn_phase[c][k] = wrap_phase(phase[c][k] + rot);
                    }
                }
                prev_phase[c].copy_from_slice(&phase[c]);
            }
        }

        // Complex Pro formant envelope preservation when transposing.
        let do_formant = formant_preserve && (pitch_ratio - 1.0).abs() > 1e-3;
        for c in 0..n_ch {
            if do_formant {
                cep_ws.compute_envelope(&mag[c], 32);
                // Reconstruct shifted spectral envelope so resampling back preserves original formants.
                let pr = pitch_ratio as f32;
                for (k, dst_env) in cep_ws.shifted_env.iter_mut().enumerate() {
                    let fk = (k as f32 / pr).clamp(0.0, (n_bins - 1) as f32);
                    let k0 = (fk.floor() as usize).min(n_bins - 1);
                    let k1 = (k0 + 1).min(n_bins - 1);
                    let t = fk - k0 as f32;
                    *dst_env = cep_ws.orig_env[k0] + (cep_ws.orig_env[k1] - cep_ws.orig_env[k0]) * t;
                }
                for ((m, &s_env), &o_env) in mag[c][..n_bins]
                    .iter_mut()
                    .zip(&cep_ws.shifted_env[..n_bins])
                    .zip(&cep_ws.orig_env[..n_bins])
                {
                    // Compensate ahead of resampling by `pitch_ratio`: at bin `k`
                    // (which resamples to `k / pitch_ratio`), place target envelope `shifted_env[k]`
                    // divided by `orig_env[k]`.
                    let gain = (s_env / o_env.max(1e-5)).clamp(0.25, 3.5);
                    *m *= gain;
                }
            }

            for k in 0..n_bins {
                let (s, co) = syn_phase[c][k].sin_cos();
                let m = mag[c][k];
                spec[c][k] = Complex::new(m * co, m * s);
            }
            spec[c][0].im = 0.0;
            spec[c][n_bins - 1].im = 0.0;
            let _ = ifft.process_with_scratch(&mut spec[c], &mut time_buf, &mut ifft_scratch);
            let write_len = fft_size.min(pv_out_len - out_pos);
            for i in 0..write_len {
                let w = window[i];
                out[c][out_pos + i] += time_buf[i] * inv_fft * w;
                if c == 0 {
                    norm[out_pos + i] += w * w;
                }
            }
        }
    }

    for ch_out in &mut out {
        for (v, &w_norm) in ch_out.iter_mut().zip(&norm) {
            if w_norm > 1e-5 {
                *v /= w_norm;
            }
        }
    }

    if (pitch_ratio - 1.0).abs() > 1e-4 || out[0].len() != out_len {
        let refs: Vec<&[f32]> = out.iter().map(|v| v.as_slice()).collect();
        resample_channels(&refs, out_len)
    } else {
        out
    }
}

// ───────────────────────────── Signalsmith Stretch (Complex / Complex Pro) ─────────────────────────────

/// Which time-stretch engine renders Complex and Complex Pro (Beats, Tones
/// and Texture are always ours: on Tones, Signalsmith's default window is
/// the same as Complex and a 60 ms one smeared chords, −3 dB of spurious
/// energy against −29.5 dB).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Stretcher {
    /// Signalsmith Stretch (MIT, crate `signalsmith-stretch`): multi-resolution
    /// phase vocoder with spectral transposition. The default when the
    /// `signalsmith` feature is on.
    #[default]
    Signalsmith,
    /// Our own phase vocoder, kept for A/B and as the fallback
    /// for clips too short for Signalsmith's window.
    Native,
}

#[cfg(feature = "signalsmith")]
mod ss {
    use signalsmith_stretch::Stretch;

    /// Largest stretch of one pass. Above a factor of 2 Signalsmith
    /// randomises each bin's phase advance with a generator seeded from
    /// `std::random_device`, which the crate's C API cannot seed: a longer
    /// stretch is done in several passes so the render is the same every
    /// time (the margin covers its per-block rounding of the input hop).
    const MAX_PASS: f64 = 1.9;

    /// Stretch planar `channels` by `time_scale` and transpose by
    /// `pitch_ratio` (formants kept with `formants`). `None` when the
    /// output is too short for the analysis window (twice its latency).
    pub(super) fn stretch(channels: &[&[f32]], sample_rate: u32, time_scale: f64, pitch_ratio: f64, formants: bool) -> Option<Vec<Vec<f32>>> {
        let n_ch = channels.len();
        let in_len = channels.first()?.len();
        if n_ch == 0 || in_len == 0 || channels.iter().any(|c| c.len() != in_len) {
            return None;
        }
        let out_len = ((in_len as f64 * time_scale).round() as usize).max(1);
        let passes = if time_scale > MAX_PASS { (time_scale.ln() / MAX_PASS.ln()).ceil() as usize } else { 1 };
        let mut cur: Vec<f32> = (0..in_len).flat_map(|i| channels.iter().map(move |c| c[i])).collect();
        for pass in 0..passes {
            let len = if pass + 1 == passes { out_len } else { ((in_len as f64 * time_scale.powf((pass + 1) as f64 / passes as f64)).round() as usize).max(1) };
            let mut st = Stretch::preset_default(n_ch as u32, sample_rate);
            if pass == 0 && pitch_ratio != 1.0 {
                st.set_transpose_factor(pitch_ratio as f32, None);
                if formants {
                    st.set_formant_factor(1.0, true);
                }
            }
            if len < 2 * st.output_latency() {
                return None;
            }
            let mut out = vec![0.0f32; len * n_ch];
            if !st.exact(&cur, &mut out) {
                return None;
            }
            cur = out;
        }
        let out: Vec<Vec<f32>> = (0..n_ch).map(|c| cur.iter().skip(c).step_by(n_ch).map(|v| if v.is_finite() { *v } else { 0.0 }).collect()).collect();
        Some(out)
    }
}

/// Offline full-quality warp (time-stretch and/or pitch-shift) for a planar buffer.
pub fn warp_audio(
    channels: &[&[f32]],
    sample_rate: u32,
    algo: WarpAlgorithm,
    time_scale: f64,
    pitch_ratio: f64,
    transients: Option<&[usize]>,
) -> Vec<Vec<f32>> {
    warp_audio_with(channels, sample_rate, algo, time_scale, pitch_ratio, transients, Stretcher::default())
}

/// [`warp_audio`] with the engine for Complex and Complex Pro chosen.
pub fn warp_audio_with(
    channels: &[&[f32]],
    sample_rate: u32,
    algo: WarpAlgorithm,
    time_scale: f64,
    pitch_ratio: f64,
    transients: Option<&[usize]>,
    stretcher: Stretcher,
) -> Vec<Vec<f32>> {
    let in_len = channels.first().map_or(0, |c| c.len());
    if in_len == 0 || sample_rate == 0 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    let time_scale = time_scale.clamp(0.125, 8.0);
    let pitch_ratio = pitch_ratio.clamp(0.0625, 16.0);
    let out_len = ((in_len as f64 * time_scale).round() as usize).max(1);

    if (time_scale - 1.0).abs() < 1e-4 && (pitch_ratio - 1.0).abs() < 1e-4 {
        return channels.iter().map(|c| c.to_vec()).collect();
    }

    match algo {
        WarpAlgorithm::Repitch => {
            let eff_out = ((in_len as f64 * (time_scale / pitch_ratio)).round() as usize).max(1);
            resample_channels(channels, eff_out)
        }
        WarpAlgorithm::Beats => {
            // Preserve transients across time stretch, then shift pitch with
            // short-window WSOLA + resample if transposed.
            let owned;
            let trans = match transients {
                Some(t) => t,
                None => {
                    owned = detect_transients(channels, sample_rate);
                    &owned
                }
            };
            let stretched = stretch_beats_slices(channels, sample_rate, time_scale, trans);
            if (pitch_ratio - 1.0).abs() < 1e-4 {
                return stretched;
            }
            let refs: Vec<&[f32]> = stretched.iter().map(|v| v.as_slice()).collect();
            let pitched_stretch = wsola_stretch(&refs, pitch_ratio, 1280, 256);
            let p_refs: Vec<&[f32]> = pitched_stretch.iter().map(|v| v.as_slice()).collect();
            resample_channels(&p_refs, out_len)
        }
        WarpAlgorithm::Tones => {
            let stretched = wsola_stretch(channels, time_scale * pitch_ratio, 1536, 320);
            if (pitch_ratio - 1.0).abs() < 1e-4 && stretched.first().is_some_and(|v| v.len() == out_len) {
                stretched
            } else {
                let refs: Vec<&[f32]> = stretched.iter().map(|v| v.as_slice()).collect();
                resample_channels(&refs, out_len)
            }
        }
        WarpAlgorithm::Texture => {
            let stretched = granular_texture_stretch(channels, sample_rate, time_scale * pitch_ratio);
            if (pitch_ratio - 1.0).abs() < 1e-4 && stretched.first().is_some_and(|v| v.len() == out_len) {
                stretched
            } else {
                let refs: Vec<&[f32]> = stretched.iter().map(|v| v.as_slice()).collect();
                resample_channels(&refs, out_len)
            }
        }
        WarpAlgorithm::Complex | WarpAlgorithm::ComplexPro => {
            let formants = algo == WarpAlgorithm::ComplexPro;
            #[cfg(feature = "signalsmith")]
            if stretcher == Stretcher::Signalsmith
                && let Some(out) = ss::stretch(channels, sample_rate, time_scale, pitch_ratio, formants)
            {
                return out;
            }
            #[cfg(not(feature = "signalsmith"))]
            let _ = stretcher;
            phase_vocoder(channels, time_scale, pitch_ratio, formants)
        }
    }
}

// ───────────────────────────── Warp along markers ─────────────────────────────

/// How far before a marker its stretch ends, seconds. Markers sit on beats,
/// and so on attacks: the cut between two stretches goes in the quiet just
/// before the hit, where the cross-fade cannot double it.
const MARKER_LEAD: f64 = 0.012;
/// Half the cross-fade between two stretches, seconds.
const MARKER_FADE: f64 = 0.004;
/// Audio rendered around each stretch so its edges have their surroundings, frames.
const MARKER_CONTEXT: usize = 4096;

/// The anchors of a time map in order: each later in the file and later in
/// the render than the one before, none past the end of the file.
fn clean_anchors(anchors: &[(usize, usize)], in_len: usize) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(anchors.len());
    for &(i, o) in anchors {
        if i > in_len {
            continue;
        }
        if out.last().is_none_or(|l| i > l.0 && o > l.1) {
            out.push((i, o));
        }
    }
    out
}

/// Offline warp along a time map: frame `anchors[k].0` of the input lands on
/// frame `anchors[k].1` of the output, and the audio between two anchors is
/// stretched to fit (each stretch by its own factor, so a tempo that drifts
/// comes out straight). The output is as long as the last anchor says; before
/// the first one it is silent. `pitch_ratio` transposes the whole as in
/// [`warp_audio`].
///
/// One factor throughout is a single [`warp_audio`] of the span. Re-Pitch
/// reads every output frame at its place on the map. The other modes render
/// each stretch with its surroundings and join them with a short cross-fade
/// placed just before the anchor's attack.
pub fn warp_audio_mapped(channels: &[&[f32]], sample_rate: u32, algo: WarpAlgorithm, anchors: &[(usize, usize)], pitch_ratio: f64, stretcher: Stretcher) -> Vec<Vec<f32>> {
    let in_len = channels.first().map_or(0, |c| c.len());
    let pts = clean_anchors(anchors, in_len);
    if in_len == 0 || sample_rate == 0 || pts.len() < 2 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    let (first, last) = (pts[0], pts[pts.len() - 1]);
    let out_len = last.1;
    let mut out: Vec<Vec<f32>> = channels.iter().map(|_| vec![0.0f32; out_len]).collect();
    let place = |out: &mut [Vec<f32>], piece: &[Vec<f32>], at: usize, len: usize| {
        for (dst, src) in out.iter_mut().zip(piece) {
            let n = len.min(src.len()).min(dst.len().saturating_sub(at));
            dst[at..at + n].copy_from_slice(&src[..n]);
        }
    };
    let overall = (last.1 - first.1) as f64 / (last.0 - first.0) as f64;
    let straight = pts.iter().all(|&(i, o)| ((first.1 as f64 + (i - first.0) as f64 * overall) - o as f64).abs() <= 1.0);
    if straight {
        let span: Vec<&[f32]> = channels.iter().map(|c| &c[first.0..last.0]).collect();
        let piece = warp_audio_with(&span, sample_rate, algo, overall, pitch_ratio, None, stretcher);
        place(&mut out, &piece, first.1, last.1 - first.1);
        return out;
    }
    if algo == WarpAlgorithm::Repitch {
        for w in pts.windows(2) {
            let ((i0, o0), (i1, o1)) = (w[0], w[1]);
            let step = (i1 - i0) as f64 / (o1 - o0) as f64;
            for (dst, src) in out.iter_mut().zip(channels) {
                for (k, v) in dst[o0..o1].iter_mut().enumerate() {
                    *v = read_cubic(src, i0 as f64 + k as f64 * step);
                }
            }
        }
        return out;
    }

    // Where one stretch hands over to the next: a little before each inner anchor.
    let lead = (MARKER_LEAD * sample_rate as f64) as usize;
    let fade = ((MARKER_FADE * sample_rate as f64) as usize).max(1);
    let mut cuts = Vec::with_capacity(pts.len());
    cuts.push(first);
    for k in 1..pts.len() - 1 {
        let (prev, here, next) = (pts[k - 1], pts[k], pts[k + 1]);
        let scale_next = (next.1 - here.1) as f64 / (next.0 - here.0) as f64;
        // Never further back than a third of the stretch it is taken from.
        let back = lead.min((here.0 - prev.0) / 3);
        let cut = (here.0 - back, here.1.saturating_sub((back as f64 * scale_next).round() as usize));
        let before = cuts[cuts.len() - 1];
        if cut.0 > before.0 && cut.1 > before.1 {
            cuts.push(cut);
        }
    }
    let before = cuts[cuts.len() - 1];
    if last.0 > before.0 && last.1 > before.1 {
        cuts.push(last);
    }
    let n = cuts.len() - 1;
    for k in 0..n {
        let ((i0, o0), (i1, o1)) = (cuts[k], cuts[k + 1]);
        let (want, scale) = (o1 - o0, (o1 - o0) as f64 / (i1 - i0) as f64);
        let (pre, post) = (MARKER_CONTEXT.min(i0), MARKER_CONTEXT.min(in_len - i1));
        let chunk: Vec<&[f32]> = channels.iter().map(|c| &c[i0 - pre..i1 + post]).collect();
        let piece = warp_audio_with(&chunk, sample_rate, algo, scale, pitch_ratio, None, stretcher);
        let core = (pre as f64 * scale).round() as isize;
        // The cross-fade reaches `fade` frames either side of a cut, where both stretches have audio.
        let f = fade.min(want / 2) as isize;
        let (head, tail) = (if k > 0 { f } else { 0 }, if k + 1 < n { f } else { 0 });
        for (dst, src) in out.iter_mut().zip(&piece) {
            for o in -head..want as isize + tail {
                let (from, to) = (core + o, o0 as isize + o);
                if from < 0 || from as usize >= src.len() || to < 0 || to as usize >= dst.len() {
                    continue;
                }
                let gain = if o < head {
                    (o + head) as f32 / (2 * head) as f32
                } else if o >= want as isize - tail {
                    1.0 - (o - (want as isize - tail)) as f32 / (2 * tail) as f32
                } else {
                    1.0
                };
                dst[to as usize] += src[from as usize] * gain;
            }
        }
    }
    out
}

// ───────────────────── Allocation-Free Real-Time Warp Readers ─────────────────────

/// Stateless, zero-allocation transient-slice reader for dynamic live BPM changes
/// or tempo ramps in `Beats` mode.
#[inline]
pub fn sample_warped_beats(slice: &[f32], transients: &[usize], pos: f64, sr: f64, reverse: bool) -> f32 {
    let len = slice.len();
    if len == 0 || !pos.is_finite() {
        return 0.0;
    }
    let idx = pos.floor() as isize;
    if idx < 0 || idx as usize >= len || transients.is_empty() {
        let p = if reverse { (len as f64 - 1.0) - pos } else { pos };
        return read_cubic(slice, p);
    }
    let i = idx as usize;
    let ti = transients.partition_point(|&t| t <= i).saturating_sub(1);
    let s0 = transients[ti].min(len - 1);
    let s1 = transients.get(ti + 1).copied().unwrap_or(len).clamp(s0 + 1, len);
    let attack = ((sr * 0.035) as usize).min((s1 - s0) / 2);
    let rel = pos - s0 as f64;
    let mapped = if rel < attack as f64 {
        s0 as f64 + rel
    } else {
        pos
    };
    let final_pos = if reverse { (len as f64 - 1.0) - mapped } else { mapped };
    read_cubic(slice, final_pos)
}

/// The two read taps of a stateless, zero-allocation pitch shifter that keeps
/// the timing of a clip (the transpose envelope of a pitch-preserving clip).
///
/// `drift` is how many frames the pitch shift has advanced the read position
/// beyond the time-aligned one (`∫ (ratio - 1)` along the clip, negative
/// when shifting down) and `window` the span the taps wander in. Each tap
/// reads at `aligned + offset`, the offsets cover `-window/2..window/2` and
/// wrap to the other end as the drift grows, where their weight is zero; the
/// two weights always sum to 1. Between wraps a tap reads the clip at exactly
/// the shifted speed, so the pitch is the ratio and the delay never exceeds
/// half a window.
#[inline]
pub fn pitch_taps(drift: f64, window: f64) -> [(f64, f32); 2] {
    let window = window.max(1.0);
    let u = if drift.is_finite() { drift / window } else { 0.0 };
    let tap = |u: f64| {
        let phase = u - u.floor();
        let s = (std::f64::consts::PI * phase).sin();
        (window * (phase - 0.5), (s * s) as f32)
    };
    [tap(u), tap(u + 0.5)]
}

/// Stateless, zero-allocation 4-overlap Hann granular reader for live tempo ramps
/// and runtime speed adjustments when no steady pre-warped buffer applies.
#[inline]
pub fn sample_warped_grain(
    slice: &[f32],
    ch_seed: u64,
    since_samples: f64,
    src_at_hop: impl Fn(f64) -> f64,
    sr: f64,
    jitter: bool,
    reverse: bool,
) -> f32 {
    let len = slice.len();
    if len == 0 || !since_samples.is_finite() {
        return 0.0;
    }
    let win = if jitter { (sr * 0.056).clamp(512.0, 4096.0) } else { (sr * 0.032).clamp(512.0, 2048.0) };
    let hop = win * 0.25;
    let m0 = (since_samples / hop).floor() as i64 - 2;
    let max_jit = if jitter { (sr * 0.015) as f32 } else { 0.0 };
    let mut sum = 0.0f32;
    let mut wsum = 0.0f32;

    for m in m0..=m0 + 4 {
        let center_out = m as f64 * hop;
        let phase = (since_samples - center_out) / win + 0.5;
        if phase <= 0.0 || phase >= 1.0 {
            continue;
        }
        let w = 0.5 - 0.5 * (TAU as f64 * phase).cos() as f32;
        let mut center_src = src_at_hop(center_out);
        if jitter {
            center_src += (splitmix_signed((m as u64).wrapping_mul(0x9E37_79B9) ^ ch_seed) * max_jit) as f64;
        }
        let mut p = center_src + (since_samples - center_out);
        if reverse {
            p = (len as f64 - 1.0) - p;
        }
        sum += read_cubic(slice, p) * w;
        wsum += w;
    }
    if wsum > 1e-5 { sum / wsum } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short decaying burst at each of `at` (frames): something every mode keeps as an attack.
    fn hits(at: &[usize], len: usize, sr: u32) -> Vec<f32> {
        let mut x = vec![0.0f32; len];
        for &a in at {
            for i in 0..(sr as usize / 20).min(len.saturating_sub(a)) {
                let t = i as f32 / sr as f32;
                x[a + i] += (TAU * 180.0 * t).sin() * (-t * 60.0).exp() * 0.8;
            }
        }
        x
    }

    /// Where each attack of `x` is: the first frame of every burst above a tenth of the peak.
    fn attacks(x: &[f32], sr: u32) -> Vec<usize> {
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let gap = sr as usize / 8;
        let mut out: Vec<usize> = Vec::new();
        for (i, v) in x.iter().enumerate() {
            if v.abs() > 0.25 * peak && out.last().is_none_or(|l| i > l + gap) {
                out.push(i);
            }
        }
        out
    }

    #[test]
    fn a_drifting_file_comes_out_straight_in_every_mode() {
        let sr = 48_000u32;
        // Eight beats that slow down: 0.50 s apart at first, 0.60 s at the end.
        let mut at = vec![2_400usize];
        for k in 0..8 {
            at.push(at[k] + ((0.50 + 0.0125 * k as f64) * sr as f64) as usize);
        }
        let len = at[8] + sr as usize / 2;
        let x = hits(&at, len, sr);
        // Each beat is wanted exactly half a second after the one before.
        let anchors: Vec<(usize, usize)> = at.iter().enumerate().map(|(k, &a)| (a, 2_400 + k * 24_000)).collect();
        for algo in [WarpAlgorithm::Repitch, WarpAlgorithm::Beats, WarpAlgorithm::Tones, WarpAlgorithm::Complex] {
            let out = warp_audio_mapped(&[&x], sr, algo, &anchors, 1.0, Stretcher::default());
            assert_eq!(out[0].len(), anchors[8].1, "{algo:?}: as long as the last anchor says");
            let found = attacks(&out[0], sr);
            assert!(found.len() >= 8, "{algo:?}: {} attacks", found.len());
            for (k, want) in anchors.iter().take(8).map(|a| a.1).enumerate() {
                let off = found.iter().map(|f| (*f as i64 - want as i64).abs()).min().unwrap();
                // Tones (WSOLA) sets each grain within its search window, 7 ms, in a plain stretch too.
                let limit_ms = if algo == WarpAlgorithm::Tones { 10 } else { 6 };
                assert!(off < sr as i64 * limit_ms / 1000, "{algo:?}: beat {k} is {} ms off", off as f64 * 1000.0 / sr as f64);
            }
            // The cross-fades leave no hole and no bump: the level between two beats stays that of a decaying hit.
            assert!(out[0].iter().all(|v| v.is_finite() && v.abs() < 1.2), "{algo:?}");
        }
    }

    #[test]
    fn a_straight_map_is_the_plain_stretch_of_its_span() {
        let sr = 48_000u32;
        let x = sine(sr, 220.0, 2.0);
        // Two anchors: the second half of the file, at 80 % of its length, from frame 1000 of the render.
        let anchors = [(48_000usize, 1_000usize), (96_000, 39_400)];
        let out = warp_audio_mapped(&[&x], sr, WarpAlgorithm::Tones, &anchors, 1.0, Stretcher::default());
        assert_eq!(out[0].len(), 39_400);
        assert!(out[0][..1_000].iter().all(|v| *v == 0.0), "silent before the first anchor");
        let plain = warp_audio(&[&x[48_000..]], sr, WarpAlgorithm::Tones, 0.8, 1.0, None);
        assert_eq!(&out[0][1_000..], &plain[0][..38_400]);
        // Three anchors on one line are still one stretch.
        let three = [(48_000usize, 1_000usize), (72_000, 20_200), (96_000, 39_400)];
        assert_eq!(warp_audio_mapped(&[&x], sr, WarpAlgorithm::Tones, &three, 1.0, Stretcher::default()), out);
    }

    #[test]
    fn anchors_out_of_order_or_too_few_are_handled() {
        let sr = 48_000u32;
        let x = sine(sr, 220.0, 1.0);
        assert!(warp_audio_mapped(&[&x], sr, WarpAlgorithm::Beats, &[(0, 0)], 1.0, Stretcher::default())[0].is_empty());
        // The anchor that goes back and the one past the end of the file are left out.
        assert_eq!(clean_anchors(&[(0, 0), (20_000, 30_000), (10_000, 40_000), (30_000, 45_000), (99_999, 60_000)], 48_000), vec![(0, 0), (20_000, 30_000), (30_000, 45_000)]);
        let out = warp_audio_mapped(&[&x], sr, WarpAlgorithm::Repitch, &[(0, 0), (24_000, 30_000), (48_000, 48_000)], 1.0, Stretcher::default());
        assert_eq!(out[0].len(), 48_000);
    }

    fn sine(sr: u32, hz: f32, secs: f32) -> Vec<f32> {
        let n = (sr as f32 * secs) as usize;
        (0..n).map(|i| 0.5 * (TAU * hz * i as f32 / sr as f32).sin()).collect()
    }

    /// Estimate dominant frequency by zero-crossings / DFT peak near expected Hz.
    fn dominant_hz(x: &[f32], sr: u32) -> f32 {
        let n = x.len().min(8192);
        let start = x.len().saturating_sub(n) / 2;
        let seg = &x[start..start + n];
        let mut crossings = 0usize;
        for w in seg.windows(2) {
            if w[0] <= 0.0 && w[1] > 0.0 {
                crossings += 1;
            }
        }
        crossings as f32 * sr as f32 / n as f32
    }

    #[test]
    fn pitch_taps_sum_to_one_and_wrap_where_their_weight_is_zero() {
        let w = 1920.0;
        let mut prev = pitch_taps(-5.0 * w, w);
        for i in -4999..5000 {
            let drift = i as f64 * w / 1000.0;
            let taps = pitch_taps(drift, w);
            assert!((taps[0].1 + taps[1].1 - 1.0).abs() < 1e-5, "weights at {drift}");
            for (t, p) in taps.iter().zip(&prev) {
                // An offset only jumps (by a whole window) while its weight is next to nothing.
                if (t.0 - p.0).abs() > w / 2.0 {
                    assert!(t.1 < 0.01 && p.1 < 0.01, "jump at {drift}: {t:?} {p:?}");
                }
                assert!(t.0.abs() <= w / 2.0 + 1e-6);
            }
            prev = taps;
        }
        assert!(pitch_taps(f64::NAN, w).iter().all(|t| t.1.is_finite()));
    }

    #[test]
    fn transient_detector_finds_impulses() {
        let sr = 48_000;
        let mut x = vec![0.0f32; sr as usize];
        for hit in [2_400usize, 14_400, 28_800, 40_000] {
            for j in 0..300 {
                x[hit + j] = (-(j as f32) / 80.0).exp() * ((j as f32) * 0.25).sin();
            }
        }
        let found = detect_transients(&[&x], sr);
        for hit in [2_400usize, 14_400, 28_800, 40_000] {
            assert!(found.iter().any(|&t| t.abs_diff(hit) < 512), "missed transient near {hit}: {found:?}");
        }
    }

    #[test]
    fn higher_sensitivity_finds_softer_onsets() {
        let sr = 48_000;
        let mut x = vec![0.0f32; sr as usize];
        // A loud hit and a quiet one a bit later.
        for (hit, level) in [(2_400usize, 0.9f32), (24_000, 0.04)] {
            for j in 0..400 {
                x[hit + j] = level * (-(j as f32) / 100.0).exp() * ((j as f32) * 0.25).sin();
            }
        }
        let low = detect_transients_with(&[&x], sr, 0.0);
        let high = detect_transients_with(&[&x], sr, 1.0);
        assert!(low.iter().any(|&t| t.abs_diff(2_400) < 512), "{low:?}");
        assert!(high.iter().any(|&t| t.abs_diff(24_000) < 512), "the soft one shows at high sensitivity: {high:?}");
        assert!(high.len() >= low.len());
    }

    #[test]
    fn all_pitch_preserving_modes_shift_pitch_without_changing_duration() {
        let sr = 48_000;
        let x = sine(sr, 440.0, 0.5);
        for algo in [WarpAlgorithm::Beats, WarpAlgorithm::Tones, WarpAlgorithm::Texture, WarpAlgorithm::Complex, WarpAlgorithm::ComplexPro] {
            let up = warp_audio(&[&x], sr, algo, 1.0, 2.0, None);
            assert_eq!(up[0].len(), x.len(), "{algo:?} keeps duration");
            assert!(up[0].iter().all(|v| v.is_finite()), "{algo:?} finite");
            let hz = dominant_hz(&up[0], sr);
            assert!((hz - 880.0).abs() < 45.0, "{algo:?} +12st expected ~880 Hz, got {hz}");
        }
    }

    #[test]
    fn all_pitch_preserving_modes_stretch_time_without_changing_pitch() {
        let sr = 48_000;
        let x = sine(sr, 440.0, 0.4);
        for algo in [WarpAlgorithm::Beats, WarpAlgorithm::Tones, WarpAlgorithm::Texture, WarpAlgorithm::Complex, WarpAlgorithm::ComplexPro] {
            let slower = warp_audio(&[&x], sr, algo, 1.5, 1.0, None);
            assert_eq!(slower[0].len(), (x.len() as f64 * 1.5).round() as usize, "{algo:?} length");
            assert!(slower[0].iter().all(|v| v.is_finite()), "{algo:?} finite");
            let hz = dominant_hz(&slower[0], sr);
            assert!((hz - 440.0).abs() < 35.0, "{algo:?} time stretch expected ~440 Hz, got {hz}");
        }
    }

    #[test]
    fn handles_tiny_and_empty_inputs_without_panics_or_nans() {
        for algo in [WarpAlgorithm::Beats, WarpAlgorithm::Tones, WarpAlgorithm::Texture, WarpAlgorithm::Complex, WarpAlgorithm::ComplexPro, WarpAlgorithm::Repitch] {
            let empty: Vec<f32> = Vec::new();
            assert!(warp_audio(&[&empty], 48_000, algo, 1.3, 1.2, None)[0].is_empty());
            let tiny = vec![0.1f32; 64];
            let out = warp_audio(&[&tiny], 48_000, algo, 1.4, 0.8, None);
            assert!(out[0].iter().all(|v| v.is_finite()));
        }
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    /// Report 24.01 §7.2 / §11 (`warpalias`): a 13 kHz sine transposed +12 st
    /// must vanish (26 kHz is past Nyquist). Before the band-limited
    /// resampler every mode folded it back to 22 kHz at full level.
    #[test]
    fn transposing_up_an_octave_does_not_alias() {
        let sr = 48_000;
        // In f64: an f32 phase this far into the signal is noisy at -50 dB.
        let x: Vec<f32> = (0..2 * sr as usize).map(|i| (0.5 * (std::f64::consts::TAU * 13_000.0 * i as f64 / sr as f64).sin()) as f32).collect();
        let rms_in = rms(&x);
        for (algo, limit) in [
            (WarpAlgorithm::Repitch, -60.0),
            (WarpAlgorithm::Tones, -60.0),
            (WarpAlgorithm::Texture, -60.0),
            (WarpAlgorithm::Beats, -60.0),
            (WarpAlgorithm::Complex, -60.0),
            (WarpAlgorithm::ComplexPro, -60.0),
        ] {
            let out = warp_audio(&[&x], sr, algo, 1.0, 2.0, None);
            let o = &out[0][8192..out[0].len() - 8192];
            let db = 20.0 * (rms(o) / rms_in).max(1e-9).log10();
            eprintln!("warp {algo:?} +12 st: 13 kHz residual {db:.1} dB");
            assert!(db < limit, "{algo:?}: 13 kHz an octave up leaves {db:.1} dB");
        }
    }


    /// Frequency from interpolated rising zero crossings (sub-Hz precision on a sine).
    fn crossing_hz(x: &[f32], sr: f64) -> f64 {
        let c: Vec<f64> = (1..x.len()).filter(|&i| x[i - 1] <= 0.0 && x[i] > 0.0).map(|i| i as f64 - 1.0 + (x[i - 1] / (x[i - 1] - x[i])) as f64).collect();
        (c.len() - 1) as f64 / (c[c.len() - 1] - c[0]) * sr
    }

    /// Report 24.01 §11 `stretch`: energy (dB) more than 6 bins away from the
    /// partials of a stationary chord, Blackman-Harris window of 65 536.
    fn spurious_db(x: &[f32], partials: &[f64]) -> f64 {
        let n = 65_536;
        let seg = &x[16_384..16_384 + n];
        let mut buf: Vec<f64> = seg
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let t = std::f64::consts::TAU * i as f64 / n as f64;
                v as f64 * (0.35875 - 0.48829 * t.cos() + 0.14128 * (2.0 * t).cos() - 0.01168 * (3.0 * t).cos())
            })
            .collect();
        let mut planner = RealFftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(n);
        let mut spec = fft.make_output_vec();
        fft.process(&mut buf, &mut spec).unwrap();
        let bin = 48_000.0 / n as f64;
        let (mut on, mut off) = (0.0, 0.0);
        for (k, c) in spec.iter().enumerate() {
            let f = k as f64 * bin;
            if partials.iter().any(|p| (f - p).abs() < 6.0 * bin) {
                on += c.norm_sqr();
            } else {
                off += c.norm_sqr();
            }
        }
        10.0 * (off / on).log10()
    }

    /// Click train every 0.5 s stretched by `scale`: (share of energy more than
    /// 2 ms before each click's peak in dB, spread of the peak positions
    /// against where the clicks belong, in samples).
    fn click_smear(out: &[f32], n: usize, scale: f64) -> (f64, i64) {
        let (mut pre, mut total) = (0.0f64, 0.0f64);
        let mut offs = Vec::new();
        let mut k = 4_800;
        while k < n - 9_600 {
            let e0 = (k as f64 * scale) as usize;
            let c = (e0 - 1_920..e0 + 1_920).max_by(|&a, &b| out[a].abs().total_cmp(&out[b].abs())).unwrap();
            offs.push(c as i64 - e0 as i64);
            for (i, v) in out.iter().enumerate().take(c + 1_920).skip(c - 1_920) {
                let e = (*v as f64).powi(2);
                total += e;
                if i + 96 < c {
                    pre += e;
                }
            }
            k += 24_000;
        }
        let spread = offs.iter().max().unwrap() - offs.iter().min().unwrap();
        (10.0 * (pre / total).log10(), spread)
    }

    fn click_train(n: usize) -> Vec<f32> {
        let mut x = vec![0.0f32; n];
        let mut k = 4_800;
        while k < n - 4_800 {
            for j in 0..48 {
                x[k + j] = (1.0 - j as f32 / 48.0) * if j % 2 == 0 { 1.0 } else { -1.0 };
            }
            k += 24_000;
        }
        x
    }

    fn chord(n: usize) -> Vec<f32> {
        let tau = std::f64::consts::TAU;
        (0..n)
            .map(|i| {
                let t = i as f64 / 48_000.0;
                (0.2 * ((tau * 220.0 * t).sin() + (tau * 277.18 * t).sin() + (tau * 329.63 * t).sin() + 0.5 * (tau * 880.0 * t).sin())) as f32
            })
            .collect()
    }

    /// Item #7: Complex and Complex Pro through Signalsmith keep the exact
    /// length and the pitch of a sine at 0.5×, 0.8×, 1.25× and 2× speed.
    #[cfg(feature = "signalsmith")]
    #[test]
    fn signalsmith_keeps_length_and_pitch_at_any_speed() {
        let sr = 48_000;
        let x = sine(sr, 440.0, 1.5);
        for speed in [0.5, 0.8, 1.25, 2.0] {
            let ts = 1.0 / speed;
            for algo in [WarpAlgorithm::Complex, WarpAlgorithm::ComplexPro] {
                let out = warp_audio(&[&x, &x], sr, algo, ts, 1.0, None);
                let n = out[0].len();
                assert_eq!(n, (x.len() as f64 * ts).round() as usize, "{algo:?} at {speed}×");
                let hz = crossing_hz(&out[0][n / 4..3 * n / 4], sr as f64);
                eprintln!("signalsmith {algo:?} {speed}×: {hz:.2} Hz");
                assert!((hz - 440.0).abs() < 0.5, "{algo:?} at {speed}×: {hz} Hz");
                let diff = out[0].iter().zip(&out[1]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
                assert!(diff < 1e-3, "linked channels stay together: {diff}");
            }
        }
        // A fifth up at unchanged length.
        let out = warp_audio(&[&x], sr, WarpAlgorithm::Complex, 1.0, 1.5, None);
        let hz = crossing_hz(&out[0][12_000..60_000], sr as f64);
        assert!((hz - 660.0).abs() < 0.5, "{hz}");
    }

    /// The render is the same every time, including stretches past 2× (done
    /// in passes, since Signalsmith randomises phases above that and its C
    /// API cannot be seeded).
    #[cfg(feature = "signalsmith")]
    #[test]
    fn signalsmith_renders_are_deterministic() {
        let x = chord(48_000);
        for (ts, pitch) in [(0.8, 1.0), (1.25, 1.5), (2.0, 1.0), (3.0, 0.75), (6.0, 1.0)] {
            for algo in [WarpAlgorithm::Complex, WarpAlgorithm::ComplexPro] {
                let a = warp_audio(&[&x], 48_000, algo, ts, pitch, None);
                let b = warp_audio(&[&x], 48_000, algo, ts, pitch, None);
                assert_eq!(a[0].len(), (x.len() as f64 * ts).round() as usize);
                assert!(a[0].iter().zip(&b[0]).all(|(p, q)| p.to_bits() == q.to_bits()), "{algo:?} ×{ts} pitch {pitch}");
                assert!(a[0].iter().all(|v| v.is_finite()));
            }
        }
    }

    /// Report 24.01 §7.2, A/B against our own vocoder: spurious energy on a
    /// stationary chord stretched ×1.25 and the timing of clicks.
    #[cfg(feature = "signalsmith")]
    #[test]
    fn signalsmith_beats_the_native_vocoder() {
        let n = 48_000 * 4;
        let x = chord(n);
        let partials = [220.0, 277.18, 329.63, 880.0];
        let clicks = click_train(n);
        let mut rows = Vec::new();
        for (name, algo, engine) in [
            ("native Complex", WarpAlgorithm::Complex, Stretcher::Native),
            ("signalsmith Complex", WarpAlgorithm::Complex, Stretcher::Signalsmith),
            ("signalsmith Complex Pro", WarpAlgorithm::ComplexPro, Stretcher::Signalsmith),
            ("native Tones", WarpAlgorithm::Tones, Stretcher::Native),
        ] {
            let tonal = warp_audio_with(&[&x], 48_000, algo, 1.25, 1.0, None, engine);
            let spur = spurious_db(&tonal[0], &partials);
            let hits = warp_audio_with(&[&clicks], 48_000, algo, 1.25, 1.0, None, engine);
            let (pre, spread) = click_smear(&hits[0], n, 1.25);
            eprintln!("{name}: chord ×1.25 spurious {spur:.1} dB, clicks pre-echo {pre:.1} dB, peak spread {spread} samples");
            rows.push((spur, pre, spread));
        }
        let (native, ss) = (rows[0], rows[1]);
        assert!(ss.0 < native.0 - 8.0, "spurious: signalsmith {:.1} dB, native {:.1} dB", ss.0, native.0);
        assert!(ss.2 < native.2, "click spread: signalsmith {}, native {}", ss.2, native.2);
        assert!(ss.1 < native.1, "pre-echo: signalsmith {:.1} dB, native {:.1} dB", ss.1, native.1);
    }

}
