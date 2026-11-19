//! Beat and downbeat tracking, and the warp markers that follow from them
//! (auto-warp).
//!
//! 1. **Where the beats are.** A spectral-flux onset envelope is searched by
//!    dynamic programming for the chain of onsets that best keeps the tempo
//!    (Ellis, "Beat Tracking by Dynamic Programming", 2007). The period it
//!    aims for follows the music: it is re-estimated in overlapping windows,
//!    so a drummer who speeds up is followed instead of being forced onto one
//!    tempo.
//! 2. **Exactly where.** The envelope has a 6 ms step and a window that sees
//!    an attack early, so every beat is then moved onto the steepest rise of
//!    the waveform around it. Beats with no clear attack keep the place the
//!    chain gave them.
//! 3. **How steady.** A line is fitted through the beats. If they all sit on
//!    it within a few milliseconds the material was made to a clock: the grid
//!    is that line (which averages the measuring error away) and the tempo is
//!    its slope. Otherwise the beats are kept as played.
//! 4. **Which beat is "one".** Kicks and chord changes fall on the first beat
//!    of the bar more than on the others: the phase with most low-end attack
//!    and most spectral change wins, with a prior for the file starting on a
//!    downbeat (loops do).
//!
//! [`auto_warp`] turns the grid into `(file frame, tick)` anchors for
//! [`aitrack_model::Warp::markers`]: two for steady material, and for
//! material that drifts as many as it takes to follow it within a few
//! milliseconds.

use aitrack_model::{PPQ, Tick};
use serde::{Deserialize, Serialize};

use crate::audio::resample_channel;
use crate::dsp::{ANALYSIS_RATE, OnsetEnvelope, Spectrogram, pick_peaks};

const N_FFT: usize = 1024;
const HOP: usize = 128;
/// Shortest material worth tracking, seconds.
const MIN_SECONDS: f64 = 2.0;
/// How strictly the chain keeps the tempo (Ellis's tightness).
const TIGHT: f32 = 100.0;
/// The same when the tempo moves: looser, so the chain can follow.
const TIGHT_DRIFTING: f32 = 45.0;
/// Local tempi within this of the global one count as one tempo.
const SAME_TEMPO: f32 = 0.012;
/// How far the local tempo may leave the global one.
const TEMPO_REACH: f32 = 0.14;
/// A beat this close to the fitted line is on it, seconds.
const ON_GRID: f64 = 0.006;
/// Markers follow the beats within this, seconds.
const MARKER_TOLERANCE: f64 = 0.004;

/// The beats of a piece of audio.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BeatGrid {
    /// Beat times in seconds, rising.
    pub beats: Vec<f64>,
    /// Index in `beats` of the first downbeat.
    pub downbeat: usize,
    /// Beats per bar the downbeat was searched with.
    pub meter: u32,
    /// Tempo: the slope of the grid when steady, else the median beat.
    pub bpm: f64,
    /// Every beat sits on one straight grid (made to a clock).
    pub steady: bool,
    /// Furthest a beat is from the straight grid, milliseconds.
    pub drift_ms: f32,
    /// Share of the beats that sit on a clear attack, 0..=1.
    pub confidence: f32,
    /// How clearly one phase of the bar stands out, 0..=1.
    pub downbeat_confidence: f32,
}

/// What [`track_beats`] may assume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatOptions {
    /// A tempo already known (file name, the library): the octave to track in.
    pub bpm_hint: Option<f64>,
    /// Beats per bar.
    pub meter: u32,
}

impl Default for BeatOptions {
    fn default() -> Self {
        Self { bpm_hint: None, meter: 4 }
    }
}

/// Track the beats of a mono signal. `None` for material that is too short,
/// silent or has no pulse.
pub fn track_beats(samples: &[f32], sample_rate: u32, opts: &BeatOptions) -> Option<BeatGrid> {
    if sample_rate == 0 || (samples.len() as f64) < MIN_SECONDS * sample_rate as f64 {
        return None;
    }
    let x = if sample_rate == ANALYSIS_RATE { samples.to_vec() } else { resample_channel(samples, sample_rate, ANALYSIS_RATE).ok()? };
    let sr = ANALYSIS_RATE;
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    if peak < 1e-4 {
        return None;
    }
    let spec = Spectrogram::compute(&x, sr, N_FFT, HOP);
    let env = beat_envelope(&spec)?;
    let fps = sr as f32 / HOP as f32;

    // The tempo to aim for, sharpened on the envelope itself (a hint from a
    // file name is a round number).
    let bpm0 = match opts.bpm_hint.filter(|b| (30.0..=300.0).contains(b)) {
        Some(b) => b as f32,
        None => crate::tempo::signal_bpm(&x, sr)?.0,
    };
    let ac = Autocorr::new(&env);
    let period0 = ac.best_period(fps * 60.0 / bpm0, 0.04, 0.02)?;
    let beat0 = period0 as f64 / fps as f64;
    let floor = peak * 0.02;
    let first_sound = x.iter().position(|v| v.abs() > floor).unwrap_or(0) as f64 / sr as f64;
    let last_sound = x.iter().rposition(|v| v.abs() > floor).unwrap_or(x.len() - 1) as f64 / sr as f64;
    let start = (first_sound - 0.01).max(0.0);
    let meter = opts.meter.clamp(1, 12);

    // Every clear attack of the file, to the millisecond, with its strength.
    let peaks = pick_peaks(&env, 0.05, ((0.06 * fps) as usize).max(1));
    let peak_times: Vec<f64> = peaks.iter().map(|&f| (f * HOP) as f64 / sr as f64).collect();
    let (onset_times, onset_clear, lead) = refine(&x, sr, &peak_times, beat0);
    let onsets: Vec<(f64, f64)> = onset_times.iter().zip(&onset_clear).zip(&peaks).filter(|((_, c), _)| **c).map(|((t, _), &f)| (*t, f64::from(env[f].min(6.0)))).collect();
    // The envelope frame an attack at `t` shows up in.
    let frame_at = |t: f64| (((t - lead) * fps as f64).round().max(0.0) as usize).min(env.len() - 1);

    // The tempo found from the envelope can be a simple ratio off (five
    // sixteenths taken for a beat). A loop is a whole number of bars long at
    // its real tempo, which settles it.
    let duration = samples.len() as f64 / sample_rate as f64;
    let (beat0, lattice) = Lattice::for_tempo(&onsets, beat0, duration, opts.bpm_hint.is_some());
    let period0 = ac.best_period(beat0 as f32 * fps, 0.01, 0.02).unwrap_or(beat0 as f32 * fps);

    // The chain of beats, each to the millisecond.
    let (periods, moving) = local_periods(&env, period0);
    let chain_frames = chain(&env, &periods, if moving { TIGHT_DRIFTING } else { TIGHT });
    let tracked = (|| {
        let coarse: Vec<f64> = chain_frames.iter().map(|&f| (f * HOP) as f64 / sr as f64).collect();
        let (times, clear, _) = refine(&x, sr, &coarse, beat0);
        // Beats before the audio starts or after it ends are the chain running on.
        let keep: Vec<bool> = times.iter().map(|&t| t >= first_sound - 0.03 && t <= last_sound + 0.03).collect();
        let (from, to) = (keep.iter().position(|k| *k)?, keep.iter().rposition(|k| *k)?);
        if to < from + 3 {
            return None;
        }
        let (times, clear, frames) = (times[from..=to].to_vec(), clear[from..=to].to_vec(), chain_frames[from..=to].to_vec());
        let fit = Fit::through(&times, &clear)?;
        Some((times, clear, frames, fit))
    })();
    let on_a_line = |a: f64, b: f64, n: usize| (0..n).map(|i| a + b * i as f64).collect::<Vec<f64>>();

    let (beats, frames, bpm, steady, drift_ms, confidence) = match (&tracked, &lattice) {
        // The beats themselves sit on one line: made to a clock, and the line
        // through them is the grid (it averages the measuring error away).
        (Some((times, clear, frames, fit)), _) if fit.steady => {
            let (a, b) = fit.snapped();
            let clear_share = clear.iter().filter(|c| **c).count() as f32 / clear.len() as f32;
            (on_a_line(a, b, times.len()), frames.clone(), 60.0 / b, true, (fit.drift * 1000.0) as f32, clear_share)
        }
        // The chain hops between rhythms (syncopation, a melody) but the
        // attacks all sit on one straight grid of sixteenths: a clock again.
        (_, Some(l)) if l.score >= STEADY_SCORE => {
            let (origin, step) = l.snapped();
            // A loop is a whole number of bars long: when the grid at exactly
            // that tempo holds as many of the attacks, it is the tempo. (Over
            // a long file a tempo half a percent off loses them all, so a
            // song that merely happens to be about that long is not touched.)
            let (origin, step) = whole_bars(duration, 4.0 * step, 0.006)
                .map(|beat| beat / 4.0)
                .and_then(|exact| Lattice::phase_at(&onsets, exact).filter(|(_, share)| *share >= l.share - 0.03).map(|(origin, _)| (origin, exact)))
                .unwrap_or((origin, step));
            // Which of the four sixteenths is the beat: the one with most onset on it.
            let beats_from = |j: usize| -> Vec<f64> {
                let k0 = ((start - origin) / step - j as f64).div_euclid(4.0).floor() as i64 * 4 + j as i64;
                (0..).map(|m| origin + (k0 + 4 * m) as f64 * step).skip_while(|t| *t < start).take_while(|t| *t <= last_sound).collect()
            };
            let strength = |beats: &[f64]| -> f32 {
                let hit: f32 = beats.iter().map(|&t| env[frame_at(t).saturating_sub(2)..(frame_at(t) + 3).min(env.len())].iter().copied().fold(0.0, f32::max)).sum();
                hit / beats.len().max(1) as f32
            };
            let options: Vec<Vec<f64>> = (0..4).map(beats_from).collect();
            let mut scores: Vec<f32> = options.iter().map(|b| strength(b)).collect();
            let top = scores.iter().copied().fold(1e-6, f32::max);
            for (sc, b) in scores.iter_mut().zip(&options) {
                // A file that starts on an attack starts on a beat, unless the others say otherwise.
                *sc = *sc / top + if b.first().is_some_and(|t| (t - first_sound).abs() < 0.025) { 0.15 } else { 0.0 };
            }
            let j = (0..4).max_by(|a, b| scores[*a].total_cmp(&scores[*b])).unwrap_or(0);
            let beats = options[j].clone();
            let frames: Vec<usize> = beats.iter().map(|&t| frame_at(t)).collect();
            (beats, frames, 15.0 / step, true, (l.spread * 1000.0) as f32, l.score.clamp(0.0, 1.0))
        }
        (Some((times, clear, frames, fit)), _) => {
            let clear_share = clear.iter().filter(|c| **c).count() as f32 / clear.len() as f32;
            let played = as_played(times, clear);
            // How far the beats are from where their neighbours put them: small for a band that drifts, large for a chain that hops between rhythms.
            let mut hops: Vec<f64> = times.iter().zip(&played).zip(clear).filter(|(_, c)| **c).map(|((t, p), _)| (t - p).abs()).collect();
            hops.sort_by(f64::total_cmp);
            let coherent = hops.get(hops.len() * 9 / 10).or(hops.last()).is_some_and(|h| *h < 0.010);
            let off_grid = lattice.as_ref().is_none_or(|l| l.score < DRIFTING_SCORE);
            if off_grid && coherent && clear_share >= 0.75 {
                // A tempo that really moves: the beats as they were played.
                let mut gaps: Vec<f64> = played.windows(2).map(|w| w[1] - w[0]).collect();
                gaps.sort_by(f64::total_cmp);
                let beat = gaps[gaps.len() / 2];
                (played, frames.clone(), 60.0 / beat, false, (fit.drift * 1000.0) as f32, clear_share)
            } else {
                // Neither clearly on a grid nor clearly drifting: one tempo
                // (the one given, else the envelope's, which a hopping chain
                // cannot spoil), a line at it through the middle of what was
                // found, and nothing bent. Being wrong about a drift would pull steady
                // audio out of time.
                let beat = opts.bpm_hint.map(|b| 60.0 / b).or(whole_bars(duration, beat0, 0.01)).unwrap_or(beat0);
                let a = fit.centre.1 - beat * fit.centre.0;
                (on_a_line(a, beat, times.len()), frames.clone(), 60.0 / beat, true, (fit.drift * 1000.0) as f32, (clear_share * 0.3).min(0.3))
            }
        }
        _ => return None,
    };
    if beats.len() < 2 || !beats.windows(2).all(|w| w[1] > w[0]) || !(20.0..=400.0).contains(&bpm) {
        return None;
    }

    // A loop is cut on the bar: when a short, steady file's grid of
    // sixteenths passes through its very first sample, that sample is the
    // "one", even if nothing sounds there (a rhythm that is all off-beats
    // gives the beat away nowhere else). Only a phase with an attack on
    // nearly every one of its beats, a pulse that can be heard, overrides it.
    let (beat, mut beats, mut frames) = (60.0 / bpm, beats, frames);
    let sixteenth = beat / 4.0;
    let from_start = beats[0] - sixteenth * (beats[0] / sixteenth).round();
    let mut opens_on_the_one = false;
    if steady && duration <= LOOP_BARS * 4.0 * beat && from_start.abs() < 0.010 {
        let zero = if from_start < 0.003 { 0.0 } else { from_start };
        let heard = |phase: f64| -> f64 {
            let on: Vec<f64> = (0..).map(|k| zero + phase + k as f64 * beat).take_while(|t| *t <= last_sound).filter(|t| *t >= start).collect();
            on.iter().filter(|t| onsets.iter().any(|o| (o.0 - **t).abs() < 0.02)).count() as f64 / on.len().max(1) as f64
        };
        let phase = (beats[0] - zero).rem_euclid(beat);
        let on_the_start = phase < 0.010 || beat - phase < 0.010;
        if on_the_start || heard(phase) < 0.75 {
            beats = (0..).map(|k| zero + k as f64 * beat).take_while(|t| *t <= last_sound).collect();
            frames = beats.iter().map(|&t| frame_at(t)).collect();
            opens_on_the_one = true;
            if beats.len() < 2 {
                return None;
            }
        }
    }

    // The grid covers all of the sound: the envelope cannot see an attack on
    // the file's very first sample, and a bar with no hit still has beats.
    // Each end carries on at the pace of its last beats.
    let (head, tail) = (beats[1] - beats[0], beats[beats.len() - 1] - beats[beats.len() - 2]);
    let before = if opens_on_the_one { 0 } else { ((beats[0] - start) / head + 1e-6).floor().max(0.0) as usize };
    let after = ((last_sound - beats[beats.len() - 1]) / tail + 1e-6).floor().max(0.0) as usize;
    let first = (beats[0] - before as f64 * head).max(0.0);
    // The phase of the bar that the first beat of the whole grid has among the tracked ones.
    let opens_on = (first - first_sound < 60.0 / bpm / 2.0).then(|| (meter as usize - before % meter as usize) % meter as usize);
    let (phase, downbeat_confidence) = if opens_on_the_one { (0, 0.8) } else { downbeat(&spec, &frames, meter as usize, opens_on) };
    let mut grid: Vec<f64> = (1..=before).rev().map(|k| beats[0] - k as f64 * head).collect();
    let last = beats[beats.len() - 1];
    grid.extend_from_slice(&beats);
    grid.extend((1..=after).map(|k| last + k as f64 * tail));
    grid.dedup_by(|b, a| *b - *a < 1e-4);
    // A line fitted through the beats can put its first ones before the file starts: there is no audio there.
    let early = grid.iter().take_while(|t| **t < 0.0).count();
    grid.drain(..early);
    if grid.len() < 2 {
        return None;
    }
    let m = meter as usize;
    let downbeat = (phase + before + m * early - early) % m;
    Some(BeatGrid { beats: grid, downbeat, meter, bpm, steady, drift_ms, confidence, downbeat_confidence })
}

// ───────────────────────────── One straight grid ─────────────────────────────

/// A steady file up to this many bars long may be a loop cut on the bar.
const LOOP_BARS: f64 = 32.0;
/// An attack this close to a grid line is on it, seconds.
const ON_LATTICE: f64 = 0.015;
/// Above this the file was made to a clock (see [`Lattice::score`]).
const STEADY_SCORE: f32 = 0.6;
/// Below this no straight grid explains the attacks: the tempo may really move.
const DRIFTING_SCORE: f32 = 0.45;

/// The tempo `beat` seconds a beat, set to the whole number of BPM it cannot
/// be told apart from given its error `err` (music made to a clock almost
/// always has one).
fn snap_beat(beat: f64, err: f64) -> f64 {
    let bpm = 60.0 / beat;
    let bpm_err = 60.0 / (beat * beat) * err;
    let round = bpm.round();
    if round > 0.0 && (bpm - round).abs() <= (2.0 * bpm_err).max(0.01) { 60.0 / round } else { beat }
}

/// The beat, near `beat`, at which a file of `duration` seconds is exactly
/// 1, 2, 4… 64 bars of four: `None` when none is within `within` (a ratio).
fn whole_bars(duration: f64, beat: f64, within: f64) -> Option<f64> {
    [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0].iter().map(|bars| duration / (4.0 * bars)).find(|b| (b / beat - 1.0).abs() < within)
}

/// A straight grid of sixteenth notes through the attacks of a file.
struct Lattice {
    /// Seconds between two lines (a sixteenth), and its standard error.
    step: f64,
    step_err: f64,
    /// The middle of the attacks on the grid, as a line number and a time: the grid turns about it.
    centre: (f64, f64),
    /// Share of the attacks (by strength) on a line, from 0 at what chance
    /// would put there to 1 for all of them.
    score: f32,
    /// The same share as it is counted, 0..=1.
    share: f64,
    /// How far the attacks on the grid are from their lines (nine in ten are nearer), seconds.
    spread: f64,
}

impl Lattice {
    /// The grid with a sixteenth near `step0` that most attacks sit on.
    fn fit(onsets: &[(f64, f64)], step0: f64) -> Option<Self> {
        let n = onsets.len();
        if n < 8 || step0 <= 0.0 {
            return None;
        }
        let total: f64 = onsets.iter().map(|o| o.1).sum();
        let tol = ON_LATTICE.min(step0 / 4.0);
        let wrap = |d: f64, s: f64| d - s * (d / s).round();
        let on_grid = |s: f64, phi: f64| onsets.iter().filter(|o| wrap(o.0 - phi, s).abs() <= tol).map(|o| o.1).sum::<f64>() / total;
        // Every step that moves the last attack by 4 ms, half a percent either way.
        let span = (onsets[n - 1].0 - onsets[0].0).max(step0);
        let reach = 0.005 * step0;
        let ds = (0.004 * step0 / span).max(2.0 * reach / 6000.0);
        let (mut best, mut step, mut origin) = (-1.0f64, step0, 0.0f64);
        let mut s = step0 - reach;
        while s <= step0 + reach {
            // The phase most attacks agree on: their mean direction around the step.
            let (mut sin, mut cos) = (0.0f64, 0.0f64);
            for o in onsets {
                let a = std::f64::consts::TAU * (o.0 / s).fract();
                sin += o.1 * a.sin();
                cos += o.1 * a.cos();
            }
            let phi = sin.atan2(cos) / std::f64::consts::TAU * s;
            let share = on_grid(s, phi);
            if share > best + 1e-9 || (share > best - 1e-9 && (s - step0).abs() < (step - step0).abs()) {
                (best, step, origin) = (share, s, phi);
            }
            s += ds;
        }
        // The line through the attacks that are on it, three times over.
        // Each pass keeps the attacks nearer to the line of the pass before:
        // one that is 10 ms late is a ghost note or a tail, and would tilt it.
        let (mut centre, mut step_err, mut spread, mut near) = ((0.0, origin), 0.0, 0.0, tol);
        for _ in 0..3 {
            let pts: Vec<(f64, f64, f64)> = onsets.iter().filter(|o| wrap(o.0 - origin, step).abs() <= near).map(|o| (((o.0 - origin) / step).round(), o.0, o.1)).collect();
            let w: f64 = pts.iter().map(|p| p.2).sum();
            if pts.len() < 6 || w <= 0.0 {
                return None;
            }
            let (mk, mt) = (pts.iter().map(|p| p.2 * p.0).sum::<f64>() / w, pts.iter().map(|p| p.2 * p.1).sum::<f64>() / w);
            let sxx: f64 = pts.iter().map(|p| p.2 * (p.0 - mk).powi(2)).sum();
            let sxy: f64 = pts.iter().map(|p| p.2 * (p.0 - mk) * (p.1 - mt)).sum();
            if sxx <= 0.0 {
                return None;
            }
            let b = sxy / sxx;
            let a = mt - b * mk;
            let mut res: Vec<f64> = pts.iter().map(|p| (p.1 - (a + b * p.0)).abs()).collect();
            let var = pts.iter().map(|p| p.2 * (p.1 - (a + b * p.0)).powi(2)).sum::<f64>() / w;
            res.sort_by(f64::total_cmp);
            spread = res[(res.len() * 9 / 10).min(res.len() - 1)];
            near = (3.0 * 1.48 * res[res.len() / 2]).clamp(0.004, tol);
            step_err = (var / sxx).sqrt();
            // The centre in the numbering of the new line: its own line 0 is at `a`.
            centre = (mk, mt);
            (step, origin) = (b, a);
        }
        let chance = (2.0 * tol / step).min(0.95);
        let share = on_grid(step, origin);
        let score = ((share - chance) / (1.0 - chance)) as f32;
        Some(Self { step, step_err, centre, score, share, spread })
    }

    /// The grid of exactly `step` seconds most attacks sit on: the time of
    /// one of its lines and the share of the attacks (by strength) on it.
    fn phase_at(onsets: &[(f64, f64)], step: f64) -> Option<(f64, f64)> {
        let total: f64 = onsets.iter().map(|o| o.1).sum();
        if onsets.is_empty() || step <= 0.0 || total <= 0.0 {
            return None;
        }
        let tol = ON_LATTICE.min(step / 4.0);
        let (mut sin, mut cos) = (0.0f64, 0.0f64);
        for o in onsets {
            let a = std::f64::consts::TAU * (o.0 / step).fract();
            sin += o.1 * a.sin();
            cos += o.1 * a.cos();
        }
        let origin = sin.atan2(cos) / std::f64::consts::TAU * step;
        let on = onsets.iter().filter(|o| ((o.0 - origin) - step * ((o.0 - origin) / step).round()).abs() <= tol).map(|o| o.1).sum::<f64>();
        Some((origin, on / total))
    }

    /// The beat to track with and its grid, for a beat of about `beat0`
    /// seconds. The tempo found from the envelope can be a simple ratio off
    /// (five sixteenths taken for a beat, a dotted rhythm for the pulse).
    /// Unless the tempo was given, its neighbours at those ratios are tried:
    /// one of them is taken when the file is a whole number of bars long at
    /// it and not at `beat0` (loops are), and its grid explains the attacks
    /// about as well. Material that is not a loop keeps the tempo found.
    fn for_tempo(onsets: &[(f64, f64)], beat0: f64, duration: f64, given: bool) -> (f64, Option<Self>) {
        let base = Self::fit(onsets, beat0 / 4.0);
        if given || whole_bars(duration, beat0, 0.01).is_some() {
            return (beat0, base);
        }
        let floor = base.as_ref().map_or(0.5, |b| (b.score - 0.1).max(0.5));
        let mut best: Option<(f64, Self)> = None;
        for ratio in [1.25, 0.8, 4.0 / 3.0, 0.75, 1.5, 2.0 / 3.0] {
            let beat = beat0 / ratio;
            if !(60.0..=200.0).contains(&(60.0 / beat)) {
                continue;
            }
            let Some(exact) = whole_bars(duration, beat, 0.01) else { continue };
            if let Some(l) = Self::fit(onsets, exact / 4.0)
                && l.score >= floor
                && best.as_ref().is_none_or(|b| l.score > b.1.score)
            {
                best = Some((exact, l));
            }
        }
        match best {
            Some((beat, l)) => (beat, Some(l)),
            None => (beat0, base),
        }
    }

    /// The grid as `(time of line 0, step)`, at a whole number of BPM when
    /// it cannot be told from one. The grid turns about the middle of its attacks.
    fn snapped(&self) -> (f64, f64) {
        let step = snap_beat(4.0 * self.step, 4.0 * self.step_err) / 4.0;
        (self.centre.1 - step * self.centre.0, step)
    }
}

/// Onset strength for finding the beat: the flux of three bands, each
/// brought to the same scale first, with the low end counting most. Summed
/// over the whole spectrum instead, a hi-hat (noise in hundreds of bins)
/// outweighs a kick (a few), and the chain settles on the off-beats.
fn beat_envelope(spec: &Spectrogram) -> Option<Vec<f32>> {
    let nyquist = spec.sample_rate as f32 / 2.0;
    let bands = [(20.0, 200.0, 1.5f32), (200.0, 2500.0, 1.0), (2500.0, nyquist, 0.5)];
    let mut env = vec![0.0f32; spec.frames.len()];
    let mut any = false;
    for (lo, hi, weight) in bands {
        let band = OnsetEnvelope::from_spectrogram(spec, lo, hi).values;
        let n = band.len().max(1) as f32;
        let mean = band.iter().sum::<f32>() / n;
        let std = (band.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n).sqrt();
        if std <= 1e-6 {
            continue;
        }
        any = true;
        for (e, v) in env.iter_mut().zip(&band) {
            *e += weight * v / std;
        }
    }
    let n = env.len().max(1) as f32;
    let mean = env.iter().sum::<f32>() / n;
    let std = (env.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n).sqrt();
    if !any || std <= 1e-6 {
        return None;
    }
    for v in &mut env {
        *v /= std;
    }
    Some(env)
}

// ───────────────────────────── Tempo, global and local ─────────────────────────────

/// Autocorrelation of the (mean-removed) onset envelope, read between lags.
struct Autocorr {
    ac: Vec<f32>,
    n: f32,
}

impl Autocorr {
    fn new(env: &[f32]) -> Self {
        let mean = env.iter().sum::<f32>() / env.len().max(1) as f32;
        let o: Vec<f32> = env.iter().map(|v| v - mean).collect();
        // Four beats at 30 BPM is the longest lag ever asked for.
        let max_lag = ((ANALYSIS_RATE as f32 / HOP as f32) * 2.0 * 4.0) as usize + 2;
        let max_lag = max_lag.min(o.len().saturating_sub(1));
        let ac0: f32 = o.iter().map(|a| a * a).sum::<f32>().max(1e-9);
        let ac = (0..=max_lag).map(|l| o[..o.len() - l].iter().zip(&o[l..]).map(|(a, b)| a * b).sum::<f32>() / ac0).collect();
        Self { ac, n: o.len() as f32 }
    }

    fn at(&self, lag: f32) -> f32 {
        let i = lag.floor() as usize;
        if i + 1 >= self.ac.len() {
            return 0.0;
        }
        let f = lag - i as f32;
        self.ac[i] * (1.0 - f) + self.ac[i + 1] * f
    }

    /// How well `period` (frames) explains the envelope: the autocorrelation
    /// at one to four beats, each corrected for the shorter overlap.
    fn score(&self, period: f32) -> f32 {
        let (mut s, mut w) = (0.0, 0.0);
        for k in 1..=4 {
            let lag = period * k as f32;
            if lag as usize + 1 >= self.ac.len() {
                break;
            }
            s += self.at(lag) * self.n / (self.n - lag).max(1.0);
            w += 1.0;
        }
        if w > 0.0 { s / w } else { 0.0 }
    }

    /// The best period within `reach` (a ratio) of `around`, searched every `step` frames.
    fn best_period(&self, around: f32, reach: f32, step: f32) -> Option<f32> {
        if !around.is_finite() || around < 4.0 {
            return None;
        }
        let (mut best, mut best_score) = (around, f32::MIN);
        let mut p = around * (1.0 - reach);
        while p <= around * (1.0 + reach) {
            let s = self.score(p);
            if s > best_score {
                (best, best_score) = (p, s);
            }
            p += step;
        }
        Some(best)
    }
}

/// The beat period (frames) at every envelope frame, and whether it moves.
/// With one tempo throughout it is `period0` everywhere.
fn local_periods(env: &[f32], period0: f32) -> (Vec<f32>, bool) {
    let n = env.len();
    let flat = (vec![period0; n], false);
    // A window of eight beats (six seconds at least), a quarter of it apart.
    let fps = ANALYSIS_RATE as f32 / HOP as f32;
    let win = ((8.0 * period0) as usize).max((6.0 * fps) as usize);
    if n < win + win / 2 {
        return flat;
    }
    let hop = (win / 4).max(1);
    let mut centres = Vec::new();
    let mut local = Vec::new();
    let mut start = 0;
    while start + win <= n {
        let ac = Autocorr::new(&env[start..start + win]);
        // A window with no pulse of its own keeps the global tempo.
        let p = ac.best_period(period0, TEMPO_REACH, 0.1).filter(|p| ac.score(*p) > 0.1).unwrap_or(period0);
        centres.push((start + win / 2) as f32);
        local.push(p);
        start += hop;
    }
    // One odd window is a fill, not a tempo change.
    let median3: Vec<f32> = (0..local.len())
        .map(|i| {
            let mut w = [local[i.saturating_sub(1)], local[i], local[(i + 1).min(local.len() - 1)]];
            w.sort_by(f32::total_cmp);
            w[1]
        })
        .collect();
    if median3.iter().all(|p| (p / period0 - 1.0).abs() < SAME_TEMPO) {
        return flat;
    }
    let mut out = Vec::with_capacity(n);
    let mut k = 0;
    for t in 0..n {
        let t = t as f32;
        while k + 1 < centres.len() && centres[k + 1] <= t {
            k += 1;
        }
        let p = if t <= centres[0] || k + 1 >= centres.len() {
            median3[k]
        } else {
            let f = (t - centres[k]) / (centres[k + 1] - centres[k]);
            median3[k] * (1.0 - f) + median3[k + 1] * f
        };
        out.push(p);
    }
    (out, true)
}

// ───────────────────────────── The chain of beats ─────────────────────────────

/// The envelope frames of the best chain of beats: each a period after the
/// one before, give or take what `tight` forgives, on as much onset as possible.
fn chain(env: &[f32], periods: &[f32], tight: f32) -> Vec<usize> {
    let n = env.len();
    let mut score = vec![0.0f32; n];
    let mut back = vec![usize::MAX; n];
    for t in 0..n {
        let period = periods[t].max(2.0);
        let lo = t as f32 - 2.0 * period;
        let hi = t as f32 - period / 2.0;
        let (mut best, mut from) = (0.0f32, usize::MAX);
        if hi >= 0.0 {
            let (a, b) = (lo.max(0.0).ceil() as usize, hi.floor() as usize);
            for (p, before) in score.iter().enumerate().take(b.min(t.saturating_sub(1)) + 1).skip(a) {
                let gap = ((t - p) as f32 / period).ln();
                let s = before - tight * gap * gap;
                if s > best {
                    (best, from) = (s, p);
                }
            }
        }
        score[t] = env[t] + best;
        back[t] = from;
    }
    // The chain ends on the best beat of the last period and a half.
    let tail = (periods.last().copied().unwrap_or(1.0) * 1.5) as usize;
    let Some(mut t) = (n.saturating_sub(tail.max(1))..n).max_by(|a, b| score[*a].total_cmp(&score[*b])) else { return Vec::new() };
    let mut beats = vec![t];
    while back[t] != usize::MAX {
        t = back[t];
        beats.push(t);
    }
    beats.reverse();
    beats
}

// ───────────────────────────── Exact positions ─────────────────────────────

/// How long the level follower holds a peak, seconds: several cycles of a
/// bass note, so the waveform's own cycles are not taken for attacks.
const HOLD: f64 = 0.08;

/// Each of the envelope's times moved onto the steepest rise of the waveform
/// near it, whether it found one, and the lead: the envelope shows an attack
/// early by a share of the analysis window that depends on the material. The
/// first pass measures that lead, the second looks around it.
fn refine(x: &[f32], sr: u32, coarse: &[f64], beat: f64) -> (Vec<f64>, Vec<bool>, f64) {
    // The level: up at once with the waveform, down slowly.
    let release = (-1.0 / (HOLD * sr as f64)).exp() as f32;
    let mut level = Vec::with_capacity(x.len());
    let mut held = 0.0f32;
    for v in x {
        held = v.abs().max(held * release);
        level.push(held);
    }
    let amp = |i: isize| -> f64 { level[i.clamp(0, x.len() as isize - 1) as usize] as f64 };
    let h = ((0.001 * sr as f64) as isize).max(1);
    // The steepest rise in [from, to] seconds: its time and how steep.
    let steepest = |from: f64, to: f64| -> Option<(f64, f64)> {
        let (a, b) = ((from * sr as f64) as isize, (to * sr as f64) as isize);
        let (a, b) = (a.max(h), b.min(x.len() as isize - h - 1));
        let (mut best, mut at) = (0.0f64, None);
        for i in a..=b {
            let rise = amp(i + h) - amp(i - h);
            if rise > best {
                (best, at) = (rise, Some(i));
            }
        }
        at.map(|i| (i as f64 / sr as f64, best))
    };
    // The lead of the envelope over the waveform, from the clearest half of the beats.
    let reach = (beat / 4.0).min(0.07);
    let first: Vec<Option<(f64, f64)>> = coarse.iter().map(|&t| steepest(t - 0.015, t + reach)).collect();
    let mut rises: Vec<f64> = first.iter().flatten().map(|f| f.1).collect();
    if rises.is_empty() {
        return (coarse.to_vec(), vec![false; coarse.len()], 0.0);
    }
    rises.sort_by(f64::total_cmp);
    let typical = rises[rises.len() / 2];
    let mut leads: Vec<f64> = coarse.iter().zip(&first).filter_map(|(&t, f)| f.filter(|f| f.1 >= typical).map(|f| f.0 - t)).collect();
    leads.sort_by(f64::total_cmp);
    let lead = leads.get(leads.len() / 2).copied().unwrap_or(0.0);
    let around = (beat / 8.0).clamp(0.012, 0.03);
    let mut times = Vec::with_capacity(coarse.len());
    let mut clear = Vec::with_capacity(coarse.len());
    for &t in coarse {
        match steepest(t + lead - around, t + lead + around).filter(|s| s.1 >= 0.25 * typical) {
            Some((at, _)) => {
                times.push(at);
                clear.push(true);
            }
            None => {
                times.push(t + lead);
                clear.push(false);
            }
        }
    }
    (times, clear, lead)
}

// ───────────────────────────── Steady or as played ─────────────────────────────

/// The straight line through the beats: `time = a + b * index`.
struct Fit {
    /// The middle of the beats the line was fitted to, as an index and a time.
    centre: (f64, f64),
    b: f64,
    /// Standard error of `b`.
    b_err: f64,
    /// Furthest a beat (smoothed over five) is from the line, seconds.
    drift: f64,
    steady: bool,
}

impl Fit {
    /// Least squares through the clear beats, with those far off the line
    /// (a ghost note taken for the beat) left out of a second fit.
    fn through(times: &[f64], clear: &[bool]) -> Option<Self> {
        let mut used: Vec<usize> = (0..times.len()).filter(|i| clear[*i]).collect();
        if used.len() < 4 {
            used = (0..times.len()).collect();
        }
        let line = |used: &[usize]| -> (f64, f64) {
            let n = used.len() as f64;
            let mi = used.iter().map(|i| *i as f64).sum::<f64>() / n;
            let mt = used.iter().map(|i| times[*i]).sum::<f64>() / n;
            let sxx: f64 = used.iter().map(|i| (*i as f64 - mi).powi(2)).sum();
            let sxy: f64 = used.iter().map(|i| (*i as f64 - mi) * (times[*i] - mt)).sum();
            let b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
            (mt - b * mi, b)
        };
        let (mut a, mut b) = line(&used);
        for _ in 0..2 {
            let mut res: Vec<f64> = used.iter().map(|i| (times[*i] - (a + b * *i as f64)).abs()).collect();
            res.sort_by(f64::total_cmp);
            let limit = (3.0 * 1.48 * res[res.len() / 2]).max(0.004);
            let kept: Vec<usize> = used.iter().copied().filter(|i| (times[*i] - (a + b * *i as f64)).abs() <= limit).collect();
            if kept.len() < 4 || kept.len() == used.len() {
                break;
            }
            used = kept;
            (a, b) = line(&used);
        }
        if b <= 0.0 {
            return None;
        }
        let res: Vec<f64> = (0..times.len()).map(|i| times[i] - (a + b * i as f64)).collect();
        // One beat off the line is that beat; five in a row are the tempo.
        let smoothed: Vec<f64> = (0..res.len())
            .map(|i| {
                let mut w: Vec<f64> = res[i.saturating_sub(2)..(i + 3).min(res.len())].to_vec();
                w.sort_by(f64::total_cmp);
                w[w.len() / 2]
            })
            .collect();
        let drift = smoothed.iter().fold(0.0f64, |m, r| m.max(r.abs()));
        let mut abs: Vec<f64> = used.iter().map(|i| res[*i].abs()).collect();
        abs.sort_by(f64::total_cmp);
        let p90 = abs[(abs.len() * 9 / 10).min(abs.len() - 1)];
        let n = used.len() as f64;
        let mi = used.iter().map(|i| *i as f64).sum::<f64>() / n;
        let sxx: f64 = used.iter().map(|i| (*i as f64 - mi).powi(2)).sum();
        let var = used.iter().map(|i| res[*i].powi(2)).sum::<f64>() / (n - 2.0).max(1.0);
        let b_err = if sxx > 0.0 { (var / sxx).sqrt() } else { 0.0 };
        let mt = used.iter().map(|i| times[*i]).sum::<f64>() / n;
        Some(Self { centre: (mi, mt), b, b_err, drift, steady: p90 < ON_GRID && drift < 1.5 * ON_GRID })
    }

    /// The line as `(time of beat 0, seconds a beat)`, at a whole number of
    /// BPM when it cannot be told from one. It turns about the middle of the
    /// beats it was fitted to.
    fn snapped(&self) -> (f64, f64) {
        let b = snap_beat(self.b, self.b_err);
        (self.centre.1 - b * self.centre.0, b)
    }
}

/// Beats as they were played: the clear ones where they are, the others (and
/// any that sits far from its neighbours' line) where the beats around them
/// put them.
fn as_played(times: &[f64], clear: &[bool]) -> Vec<f64> {
    let n = times.len();
    let mut out = times.to_vec();
    for i in 0..n {
        let (a, b) = (i.saturating_sub(4), (i + 5).min(n));
        let near: Vec<usize> = (a..b).filter(|j| *j != i && clear[*j]).collect();
        if near.len() < 3 {
            continue;
        }
        let m = near.len() as f64;
        let mi = near.iter().map(|j| *j as f64).sum::<f64>() / m;
        let mt = near.iter().map(|j| times[*j]).sum::<f64>() / m;
        let sxx: f64 = near.iter().map(|j| (*j as f64 - mi).powi(2)).sum();
        let sxy: f64 = near.iter().map(|j| (*j as f64 - mi) * (times[*j] - mt)).sum();
        if sxx <= 0.0 {
            continue;
        }
        let expected = mt + sxy / sxx * (i as f64 - mi);
        if !clear[i] || (times[i] - expected).abs() > 0.015 {
            out[i] = expected;
        }
    }
    // Never out of order, whatever was replaced.
    for i in 1..n {
        if out[i] <= out[i - 1] {
            out[i] = out[i - 1] + 1e-3;
        }
    }
    out
}

// ───────────────────────────── Downbeats ─────────────────────────────

/// Index of the first downbeat among the beats at envelope `frames`, and how
/// clearly its phase stands out. `opens_on` is the phase the sound starts on,
/// when it starts on a beat.
fn downbeat(spec: &Spectrogram, frames: &[usize], meter: usize, opens_on: Option<usize>) -> (usize, f32) {
    let n = frames.len();
    if meter < 2 || n < meter + 1 {
        return (opens_on.unwrap_or(0) % meter.max(1), 0.0);
    }
    // Low-end attack at each beat: kicks and bass notes.
    let low = OnsetEnvelope::from_spectrogram(spec, 20.0, 160.0).values;
    let low_at: Vec<f32> = frames.iter().map(|&f| low[f.saturating_sub(2)..(f + 6).min(low.len())].iter().copied().fold(0.0, f32::max)).collect();
    // How much the sound changes at each beat: the spectrum of the beat that
    // starts there against the one that ends there, in a few wide bands.
    let (lo, hi) = (spec.hz_bin(60.0).max(1), spec.hz_bin(4000.0));
    const BANDS: usize = 40;
    let band_of = |bin: usize| ((bin - lo) * BANDS / (hi - lo).max(1)).min(BANDS - 1);
    let picture = |from: usize, to: usize| -> [f32; BANDS] {
        let mut v = [0.0f32; BANDS];
        let (from, to) = (from.min(spec.frames.len()), to.min(spec.frames.len()));
        for frame in &spec.frames[from..to] {
            for bin in lo..hi {
                v[band_of(bin)] += (1.0 + 100.0 * frame[bin]).ln();
            }
        }
        v
    };
    let change_at: Vec<f32> = (0..n)
        .map(|i| {
            if i == 0 || i + 1 >= n {
                return 0.0;
            }
            let (before, after) = (picture(frames[i - 1], frames[i]), picture(frames[i], frames[i + 1]));
            let dot: f32 = before.iter().zip(&after).map(|(a, b)| a * b).sum();
            let (na, nb) = (before.iter().map(|a| a * a).sum::<f32>().sqrt(), after.iter().map(|a| a * a).sum::<f32>().sqrt());
            if na > 0.0 && nb > 0.0 { 1.0 - dot / (na * nb) } else { 0.0 }
        })
        .collect();
    let z = |v: &[f32]| -> Vec<f32> {
        let m = v.iter().sum::<f32>() / v.len() as f32;
        let s = (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt();
        v.iter().map(|x| if s > 1e-9 { (x - m) / s } else { 0.0 }).collect()
    };
    let (low_z, change_z) = (z(&low_at), z(&change_at));
    let mut scores: Vec<f32> = (0..meter)
        .map(|phase| {
            let idx: Vec<usize> = (phase..n).step_by(meter).collect();
            idx.iter().map(|&i| low_z[i] + change_z[i]).sum::<f32>() / idx.len().max(1) as f32
        })
        .collect();
    // A file that starts on a beat starts, more often than not, on the "one".
    if let Some(phase) = opens_on {
        scores[phase % meter] += 0.35;
    }
    let best = (0..meter).max_by(|a, b| scores[*a].total_cmp(&scores[*b])).unwrap_or(0);
    let second = (0..meter).filter(|p| *p != best).map(|p| scores[p]).fold(f32::MIN, f32::max);
    let margin = ((scores[best] - second) / (scores[best].abs() + second.abs() + 0.5)).clamp(0.0, 1.0);
    (best, margin)
}

// ───────────────────────────── Warp markers ─────────────────────────────

/// What auto-warp sets on a file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoWarp {
    /// The file's tempo.
    pub bpm: f64,
    /// `(file frame, source tick)` anchors, rising: a beat is `PPQ` ticks.
    pub markers: Vec<(u64, Tick)>,
    /// Source tick of the first downbeat: where a clip starts to sit on the bar.
    pub downbeat_tick: Tick,
    pub steady: bool,
    pub drift_ms: f32,
    pub confidence: f32,
    pub downbeat_confidence: f32,
}

/// The warp markers of a beat grid, for a file at `sample_rate`. Source
/// ticks count from the file's start at the grid's tempo, so the first
/// downbeat keeps the place it has in an unwarped clip; every beat after it
/// is one quarter note on. A steady grid needs two markers; one that drifts
/// gets as many as it takes for every beat to be within a few milliseconds
/// of the line between its neighbours.
pub fn auto_warp(grid: &BeatGrid, sample_rate: u32) -> Option<AutoWarp> {
    let n = grid.beats.len();
    if n < 2 || sample_rate == 0 || grid.bpm <= 0.0 {
        return None;
    }
    let d = grid.downbeat.min(n - 1);
    let downbeat_tick = (grid.beats[d] * grid.bpm / 60.0 * PPQ as f64).round() as Tick;
    let tick = |i: usize| downbeat_tick + (i as Tick - d as Tick) * PPQ;
    let frame = |i: usize| (grid.beats[i] * sample_rate as f64).round().max(0.0);
    // Beats the count would put before the file's start anchor nothing.
    let first = (0..n).find(|i| tick(*i) >= 0)?;
    let last = n - 1;
    if last <= first {
        return None;
    }
    let mut keep = vec![first, last];
    if !grid.steady {
        // Add the beat furthest from the line between two markers until none is far.
        let tolerance = MARKER_TOLERANCE * sample_rate as f64;
        let mut spans = vec![(first, last)];
        while let Some((a, b)) = spans.pop() {
            let (fa, fb) = (frame(a), frame(b));
            let worst = (a + 1..b).map(|i| (i, (frame(i) - (fa + (fb - fa) * (i - a) as f64 / (b - a) as f64)).abs())).max_by(|x, y| x.1.total_cmp(&y.1));
            if let Some((i, _)) = worst.filter(|w| w.1 > tolerance) {
                keep.push(i);
                spans.push((a, i));
                spans.push((i, b));
            }
        }
        keep.sort_unstable();
    }
    let mut markers: Vec<(u64, Tick)> = Vec::with_capacity(keep.len());
    for i in keep {
        let m = (frame(i) as u64, tick(i));
        if markers.last().is_none_or(|l| m.0 > l.0 && m.1 > l.1) {
            markers.push(m);
        }
    }
    if markers.len() < 2 {
        return None;
    }
    Some(AutoWarp { bpm: grid.bpm, markers, downbeat_tick, steady: grid.steady, drift_ms: grid.drift_ms, confidence: grid.confidence, downbeat_confidence: grid.downbeat_confidence })
}

/// Decode `path`, track its beats and build its warp markers.
pub fn auto_warp_file(path: impl AsRef<std::path::Path>, opts: &BeatOptions) -> crate::Result<Option<AutoWarp>> {
    let audio = crate::audio::load(path)?;
    let mono = audio.to_mono();
    Ok(track_beats(&mono, audio.sample_rate, opts).and_then(|g| auto_warp(&g, audio.sample_rate)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    const SR: u32 = 44_100;

    /// A drum loop with its beats at `beats` (seconds): a kick on every beat,
    /// a snare on the second and fourth of each bar counted from `one`, hats
    /// on the off-beats, and a bass note that changes on each "one".
    fn drums(beats: &[f64], one: usize, secs: f64) -> Vec<f32> {
        let mut x = vec![0.0f32; (secs * SR as f64) as usize];
        // The tails fade out: a sample cut short clicks, and the click is an onset of its own.
        let faded = |mut v: Vec<f32>| {
            let n = v.len();
            for (i, s) in v.iter_mut().enumerate() {
                *s *= (1.0 - i as f32 / n as f32).powi(2);
            }
            v
        };
        let (kick, snare) = (faded(synth::kick(SR, 0.25)), faded(synth::snare(SR, 0.18, 7)));
        let notes = [55.0f32, 65.4, 49.0, 73.4];
        for (i, &t) in beats.iter().enumerate() {
            let at = (t * SR as f64).round() as usize;
            let in_bar = (i + 4 - one % 4) % 4;
            synth::mix_at(&mut x, &kick, at);
            if in_bar == 1 || in_bar == 3 {
                synth::mix_at(&mut x, &snare, at);
            }
            if in_bar == 0 {
                let bar = (i + 4 - one % 4) / 4;
                let len = beats.get(i + 4).map_or(1.5, |n| n - t) as f32;
                let note = faded(synth::tone(notes[bar % notes.len()], len, SR, &[0.5, 0.25, 0.12]).into_iter().map(|v| v * 0.35).collect());
                synth::mix_at(&mut x, &note, at);
            }
            if let Some(next) = beats.get(i + 1) {
                synth::mix_at(&mut x, &synth::hat(SR, 0.05, i as u32 + 1), ((t + (next - t) / 2.0) * SR as f64).round() as usize);
            }
        }
        x
    }

    fn straight(bpm: f64, start: f64, n: usize) -> Vec<f64> {
        (0..n).map(|i| start + i as f64 * 60.0 / bpm).collect()
    }

    /// The largest distance from a true beat to the nearest tracked one, ms.
    fn worst_ms(found: &[f64], truth: &[f64]) -> f64 {
        truth.iter().map(|t| found.iter().map(|f| (f - t).abs()).fold(f64::MAX, f64::min)).fold(0.0, f64::max) * 1000.0
    }

    #[test]
    fn a_steady_loop_gives_its_tempo_and_its_beats_to_the_millisecond() {
        for (bpm, start) in [(128.0, 0.0), (126.5, 0.0), (174.0, 0.31), (92.0, 0.12)] {
            let truth = straight(bpm, start, 32);
            let x = drums(&truth, 0, truth[31] + 60.0 / bpm);
            let g = track_beats(&x, SR, &BeatOptions::default()).unwrap_or_else(|| panic!("{bpm}: no grid"));
            assert!(g.steady, "{bpm}: {g:?}");
            assert!((g.bpm - bpm).abs() < 0.05, "{bpm}: found {}", g.bpm);
            assert!(worst_ms(&g.beats, &truth) < 2.5, "{bpm}: {} ms off", worst_ms(&g.beats, &truth));
            assert!(g.confidence > 0.8, "{bpm}: {}", g.confidence);
        }
    }

    #[test]
    fn a_whole_number_tempo_is_given_as_one() {
        let truth = straight(128.0, 0.0, 64);
        let g = track_beats(&drums(&truth, 0, 30.5), SR, &BeatOptions::default()).unwrap();
        assert_eq!(g.bpm, 128.0);
        // A tempo that is not whole is not rounded to one.
        let truth = straight(127.4, 0.0, 64);
        let g = track_beats(&drums(&truth, 0, 30.6), SR, &BeatOptions::default()).unwrap();
        assert!((g.bpm - 127.4).abs() < 0.03 && g.bpm != 127.0, "{}", g.bpm);
    }

    #[test]
    fn a_tempo_that_speeds_up_is_followed_beat_by_beat() {
        // 100 → 112 BPM over 48 beats: a band that rushes.
        let mut truth = vec![0.2f64];
        for i in 1..48 {
            let bpm = 100.0 + 12.0 * i as f64 / 47.0;
            truth.push(truth[i - 1] + 60.0 / bpm);
        }
        let x = drums(&truth, 0, truth[47] + 0.6);
        let g = track_beats(&x, SR, &BeatOptions::default()).unwrap();
        assert!(!g.steady, "{g:?}");
        assert!(g.drift_ms > 50.0, "{}", g.drift_ms);
        assert!(worst_ms(&g.beats, &truth) < 6.0, "{} ms off", worst_ms(&g.beats, &truth));
        let w = auto_warp(&g, SR).unwrap();
        assert!(w.markers.len() > 4, "a drifting tempo needs markers along the way: {}", w.markers.len());
        // Between two markers every true beat is where the line between them says, within a few ms.
        let frame_of = |tick: f64| -> f64 {
            let k = w.markers.windows(2).position(|m| tick <= m[1].1 as f64).unwrap_or(w.markers.len() - 2);
            let (a, b) = (w.markers[k], w.markers[k + 1]);
            a.0 as f64 + (b.0 - a.0) as f64 * (tick - a.1 as f64) / (b.1 - a.1) as f64
        };
        let first = g.beats.iter().position(|b| (b - truth[0]).abs() < 0.01).expect("the first beat is tracked");
        for (i, t) in truth.iter().enumerate() {
            let tick = (w.downbeat_tick + (first as Tick + i as Tick - g.downbeat as Tick) * PPQ) as f64;
            if tick < w.markers[0].1 as f64 || tick > w.markers[w.markers.len() - 1].1 as f64 {
                continue;
            }
            let off_ms = (frame_of(tick) / SR as f64 - t).abs() * 1000.0;
            assert!(off_ms < 9.0, "beat {i}: {off_ms} ms");
        }
    }

    #[test]
    fn the_one_is_found_when_the_file_starts_on_another_beat() {
        // Three beats of pick-up, then the bar: the "one" is the fourth beat.
        let truth = straight(120.0, 0.05, 35);
        let x = drums(&truth, 3, truth[34] + 0.5);
        let g = track_beats(&x, SR, &BeatOptions::default()).unwrap();
        let first = g.beats.iter().position(|b| (b - truth[0]).abs() < 0.01).expect("the first beat is tracked");
        assert_eq!((g.downbeat + 4 - first % 4) % 4, 3, "{g:?}");
        // And a loop that starts on the bar keeps its first beat as the "one".
        let truth = straight(120.0, 0.0, 32);
        let g = track_beats(&drums(&truth, 0, 16.0), SR, &BeatOptions::default()).unwrap();
        assert_eq!(g.downbeat % 4, 0, "{g:?}");
    }

    #[test]
    fn a_loop_of_off_beats_keeps_its_one_on_the_first_sample() {
        // Nothing on any beat: hits on the last sixteenth of beat one and the "and" of beat two, every two beats.
        let beat = 60.0 / 126.0;
        let mut x = vec![0.0f32; (16.0 * beat * SR as f64) as usize];
        let hit: Vec<f32> = synth::kick(SR, 0.2).into_iter().enumerate().map(|(i, v)| v * (1.0 - i as f32 / (0.2 * SR as f32)).powi(2)).collect();
        for pair in 0..8 {
            for at in [0.75, 1.5] {
                synth::mix_at(&mut x, &hit, ((pair as f64 * 2.0 + at) * beat * SR as f64).round() as usize);
            }
        }
        let g = track_beats(&x, SR, &BeatOptions::default()).unwrap();
        assert!(g.steady && (g.bpm - 126.0).abs() < 0.05, "{g:?}");
        assert!(g.beats[0].abs() < 0.003, "the grid starts with the file: {}", g.beats[0]);
        assert_eq!(g.downbeat, 0);
        assert!(g.downbeat_confidence > 0.5);
        // The same hits with a kick on every beat, the file starting half a beat early: the kicks are the beat.
        let mut y = vec![0.0f32; (16.5 * beat * SR as f64) as usize];
        for k in 0..16 {
            synth::mix_at(&mut y, &hit, ((k as f64 + 0.5) * beat * SR as f64).round() as usize);
        }
        let g = track_beats(&y, SR, &BeatOptions::default()).unwrap();
        assert!((g.beats[0] - 0.5 * beat).abs() < 0.004, "a pulse that can be heard is the beat: {}", g.beats[0]);
    }

    #[test]
    fn a_steady_grid_is_two_markers_on_the_file_s_own_time() {
        let truth = straight(125.0, 0.24, 40);
        let g = track_beats(&drums(&truth, 0, truth[39] + 0.5), SR, &BeatOptions::default()).unwrap();
        let w = auto_warp(&g, SR).unwrap();
        assert_eq!(w.markers.len(), 2);
        assert_eq!(w.bpm, 125.0);
        // The first downbeat keeps the place it has at the file's tempo: 0.24 s is half a beat at 125 BPM.
        assert!((w.downbeat_tick - PPQ / 2).abs() <= 2, "{}", w.downbeat_tick);
        // The markers are on that same straight line: warping bends nothing.
        for (frame, tick) in &w.markers {
            let linear = *frame as f64 / SR as f64 * w.bpm / 60.0 * PPQ as f64;
            assert!((linear - *tick as f64).abs() < 3.0, "{frame} {tick}");
        }
        assert_eq!((w.markers[1].1 - w.markers[0].1) % PPQ, 0, "markers sit on beats");
    }

    #[test]
    fn silence_and_tiny_files_give_nothing() {
        assert_eq!(track_beats(&vec![0.0; SR as usize * 4], SR, &BeatOptions::default()), None);
        assert_eq!(track_beats(&synth::kick(SR, 0.5), SR, &BeatOptions::default()), None);
        let grid = BeatGrid { beats: vec![1.0], downbeat: 0, meter: 4, bpm: 120.0, steady: true, drift_ms: 0.0, confidence: 1.0, downbeat_confidence: 0.0 };
        assert_eq!(auto_warp(&grid, SR), None);
    }

    #[test]
    fn a_hint_keeps_the_octave() {
        // Half-time material: a hit every other beat of 140.
        let truth = straight(70.0, 0.0, 24);
        let x = drums(&truth, 0, truth[23] + 0.9);
        let g = track_beats(&x, SR, &BeatOptions { bpm_hint: Some(140.0), meter: 4 }).unwrap();
        assert!((g.bpm - 140.0).abs() < 0.2, "{}", g.bpm);
        let g = track_beats(&x, SR, &BeatOptions { bpm_hint: Some(70.0), meter: 4 }).unwrap();
        assert!((g.bpm - 70.0).abs() < 0.1, "{}", g.bpm);
    }

    /// Real loops against the tempo in their file names. Not part of the
    /// suite: `AITRACK_BEATS_DIR=/folder/of/loops cargo test -p aitrack-ai
    /// real_loops -- --ignored --nocapture` prints one line per file.
    #[test]
    #[ignore = "needs a folder of loops: AITRACK_BEATS_DIR"]
    fn real_loops_match_the_tempo_in_their_names() {
        let Some(dir) = std::env::var_os("AITRACK_BEATS_DIR") else { return };
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav") || x.eq_ignore_ascii_case("aif") || x.eq_ignore_ascii_case("aiff") || x.eq_ignore_ascii_case("flac") || x.eq_ignore_ascii_case("mp3")) {
                    files.push(p);
                }
            }
        }
        files.sort();
        let (mut named, mut right, mut steady, mut none, mut hinted, mut hinted_steady) = (0, 0, 0, 0, 0, 0);
        for f in files {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            let Some(want) = crate::tempo::bpm_from_filename(&name) else { continue };
            let Ok(audio) = crate::audio::load(&f) else { continue };
            if audio.duration() < MIN_SECONDS {
                continue;
            }
            named += 1;
            let mono = audio.to_mono();
            if let Some(g) = track_beats(&mono, audio.sample_rate, &BeatOptions { bpm_hint: Some(f64::from(want.bpm)), meter: 4 }) {
                hinted += usize::from((g.bpm - f64::from(want.bpm)).abs() < 0.15);
                hinted_steady += usize::from(g.steady);
            }
            match track_beats(&mono, audio.sample_rate, &BeatOptions::default()) {
                Some(g) => {
                    let w = auto_warp(&g, audio.sample_rate);
                    let folded = [1.0, 2.0, 0.5].iter().map(|m| g.bpm * m).min_by(|a, b| (a - want.bpm as f64).abs().total_cmp(&(b - want.bpm as f64).abs())).unwrap();
                    let ok = (folded - want.bpm as f64).abs() < 0.15;
                    right += usize::from(ok);
                    steady += usize::from(g.steady);
                    println!(
                        "{} {name}: want {} got {:.3} steady {} drift {:.1} ms conf {:.2} first beat {:.1} ms downbeat {} ({:.2}) markers {}",
                        if ok { "ok  " } else { "MISS" },
                        want.bpm,
                        g.bpm,
                        g.steady,
                        g.drift_ms,
                        g.confidence,
                        g.beats[0] * 1000.0,
                        g.downbeat,
                        g.downbeat_confidence,
                        w.map_or(0, |w| w.markers.len())
                    );
                }
                None => {
                    none += 1;
                    println!("none {name}: want {}", want.bpm);
                }
            }
        }
        println!("{named} loops with a tempo in their name: {right} right, {steady} steady, {none} with no grid; given the tempo: {hinted} right, {hinted_steady} steady");
    }
}
