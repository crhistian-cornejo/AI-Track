//! Compiled clips and the real-time clip player.
//!
//! A MIDI clip is compiled into an *intro* section (from the clip offset to
//! the loop end, or the whole clip when it does not loop) followed by an
//! optional *cycle* section (the loop region) that repeats until the clip
//! ends — forever in the Session View. Notes are emitted with sample offsets
//! computed through the tempo map, so ramps are honoured exactly.
//!
//! Note-offs are not read from the clip: every note-on records its off tick
//! in the track's [`ActiveNotes`], which survives graph swaps, loop wraps
//! and clip launches, so notes never hang when the project is edited.
//!
//! Audio clips play decoded, resampled data from the sample cache. Without
//! warp and transpose they play at their native speed, reading whole frames
//! (looping or not). With `warp.enabled` and an `original_bpm` they are
//! *repitched* (speed follows the tempo); transpose and detune repitch on
//! top, warped or not, as Live's Re-Pitch does. A reversed clip reads its
//! source backwards. Pitch-preserving stretch plugs in at
//! [`AudioClipData::source_frame`].
//!
//! The gain of an audio clip is its `gain_db` times its fades and volume
//! envelope ([`ClipShape`], the same curve as `Clip::gain_at`), evaluated per
//! sample on the clip's timeline. A looping clip crossfades for a few
//! milliseconds into the material that leads up to the loop start every time
//! it wraps (into the loop's first frame when the loop starts at the
//! beginning of the file), so a sample that does not loop perfectly does not
//! click.

use crate::sample_cache::{DecodedAudio, SampleCache};
use aitrack_dsp::event::Expression;
use aitrack_dsp::warp::{pitch_taps, sample_warped_beats, sample_warped_grain};
use aitrack_dsp::{AudioBuffer, Event, EventKind};
use aitrack_model::groove::{grooved_velocity, random_unit, Groove, GrooveParams, GrooveSettings, NoteGroove, PoolGroove, MAX_GROOVE_AMOUNT};
use aitrack_model::{clip_pan_gains, curve_shape, AutomationPoint, Clip, ClipEnv, ClipId, Fade, FollowAction, GrooveId, LaunchMode, NoteSource, Quantize, TempoMap, Tick, WarpMode, PPQ};
use std::sync::Arc;

/// Musical-time context of one processing segment.
pub(crate) struct TimeCtx<'a> {
    pub tempo: &'a TempoMap,
    pub sr: f64,
    /// First sample of the segment.
    pub seg_start: i64,
}

impl TimeCtx<'_> {
    /// First sample at or after `tick`.
    #[inline]
    pub fn sample_of(&self, tick: Tick) -> i64 {
        (self.tempo.tick_to_samples(tick, self.sr) - 1e-6).ceil() as i64
    }

    /// [`sample_of`](Self::sample_of) for a fractional tick (a groove moves
    /// notes by less than a tick): the first sample at or after it.
    #[inline]
    pub fn sample_of_f(&self, tick: f64) -> i64 {
        let whole = tick.floor();
        if whole == tick {
            return self.sample_of(whole as Tick);
        }
        let ti = whole as Tick;
        let secs = self.tempo.tick_to_seconds(ti) + (tick - whole) * 60.0 / (self.tempo.bpm_at(ti) * PPQ as f64);
        (secs * self.sr - 1e-6).ceil() as i64
    }

    #[inline]
    pub fn tick_of(&self, sample: i64) -> f64 {
        self.tempo.samples_to_tick(sample as f64, self.sr)
    }
}

// ───────────────────────────── Events ─────────────────────────────

/// Fixed-capacity event list (never grows on the audio thread).
pub(crate) struct EventList {
    pub events: Vec<Event>,
    pub dropped: u32,
}

impl EventList {
    pub fn with_capacity(n: usize) -> Self {
        Self { events: Vec::with_capacity(n), dropped: 0 }
    }

    #[inline]
    pub fn push(&mut self, offset: u32, kind: EventKind) {
        if self.events.len() < self.events.capacity() {
            self.events.push(Event { offset, kind });
        } else {
            self.dropped += 1;
        }
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Slots left before the list is full.
    #[inline]
    pub fn free(&self) -> usize {
        self.events.capacity() - self.events.len()
    }

    /// Sort by offset; at equal offsets note-offs come before note-ons so a
    /// retriggered key is not cut. In place, no allocation.
    pub fn sort(&mut self) {
        self.events.sort_unstable_by_key(|e| (e.offset, priority(&e.kind)));
    }
}

fn priority(kind: &EventKind) -> u8 {
    match kind {
        EventKind::AllNotesOff => 0,
        EventKind::NoteOff { .. } => 1,
        EventKind::Param { .. } | EventKind::ControlChange { .. } | EventKind::PitchBend { .. } => 2,
        EventKind::NoteOn { .. } => 3,
        _ => 4,
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveNote {
    pub key: u8,
    pub note_id: i32,
    /// Where the note ends, in ticks (fractional: a groove moves it by less than a tick).
    pub off_tick: f64,
    /// Never release before this sample (zero-length notes).
    pub min_off: i64,
    pub release: f32,
}

/// Notes currently sounding on a track, with their scheduled ends.
pub(crate) struct ActiveNotes {
    pub notes: Vec<ActiveNote>,
}

impl ActiveNotes {
    pub fn new() -> Self {
        Self { notes: Vec::with_capacity(512) }
    }

    #[cfg(test)]
    fn with_capacity(n: usize) -> Self {
        Self { notes: Vec::with_capacity(n) }
    }

    /// Emit note-offs whose time falls before `hi` (stale ones at offset 0).
    pub fn release_due(&mut self, ctx: &TimeCtx<'_>, hi: i64, out: &mut EventList) {
        let mut i = 0;
        while i < self.notes.len() {
            let n = self.notes[i];
            let at = ctx.sample_of_f(n.off_tick).max(n.min_off);
            if at < hi {
                let offset = (at - ctx.seg_start).max(0) as u32;
                out.push(offset, EventKind::NoteOff { channel: 0, key: n.key, velocity: n.release, note_id: n.note_id });
                self.notes.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Release everything at `offset` (stop, seek, loop wrap, clip change).
    pub fn release_all(&mut self, offset: u32, out: &mut EventList) {
        for n in self.notes.drain(..) {
            out.push(offset, EventKind::NoteOff { channel: 0, key: n.key, velocity: n.release, note_id: n.note_id });
        }
    }

    /// Register a note that is about to sound and, when it overlaps a held note
    /// on its key, end the old one. `events` is how many events the caller
    /// will push for the new note (its note-on and expressions).
    ///
    /// Returns `false`, touching nothing, when the note must not sound: the
    /// list of held notes is full, or the event list could not take the
    /// note's events *and* still have a slot for the note-off of every held
    /// note. That reserve (`len + held ≤ capacity` at all times) is what lets
    /// a note-off always be written, so a note-on is never left hanging.
    #[must_use]
    fn start(&mut self, note: ActiveNote, out: &mut EventList, offset: u32, events: usize) -> bool {
        let old = self.notes.iter().position(|n| n.key == note.key);
        if self.notes.len() - usize::from(old.is_some()) >= self.notes.capacity() || out.free() < events + self.notes.len() + 1 {
            out.dropped += 1;
            return false;
        }
        // A new note on a key that is still held (overlap): end the old one.
        if let Some(i) = old {
            let old = self.notes.swap_remove(i);
            out.push(offset, EventKind::NoteOff { channel: 0, key: old.key, velocity: old.release, note_id: old.note_id });
        }
        self.notes.push(note);
        true
    }
}

/// Tiny deterministic RNG for note probability (xorshift32).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rng(pub u32);

impl Rng {
    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

// ───────────────────────────── Compiled clips ─────────────────────────────

#[derive(Clone, Copy, Debug)]
pub(crate) struct SNote {
    /// Relative to the section start.
    pub start: Tick,
    /// Relative, already truncated to the section end.
    pub end: Tick,
    pub key: u8,
    pub velocity: f32,
    pub release: f32,
    pub probability: f32,
    pub note_id: i32,
    pub pan: f32,
    pub fine_pitch: f32,
    /// What the clip's groove does to this note, before settings and amount
    /// (all zero without a groove).
    pub groove: NoteGroove,
}

#[derive(Debug, Default)]
pub(crate) struct Section {
    pub len: Tick,
    pub notes: Vec<SNote>,
}

impl Section {
    fn from_notes(src: &NoteSource, from: Tick, to: Tick, groove: Option<&Groove>) -> Self {
        let mut notes: Vec<SNote> = src
            .notes
            .iter()
            .filter(|n| !n.muted && n.start >= from && n.start < to && n.probability > 0.0)
            .map(|n| SNote {
                start: n.start - from,
                end: (n.end().min(to) - from).max(n.start - from),
                key: n.key.min(127),
                velocity: n.velocity.clamp(0.0, 1.0),
                release: n.release,
                probability: n.probability,
                note_id: n.id.0 as i32,
                pan: n.pan,
                fine_pitch: n.fine_pitch,
                groove: groove.map_or_else(NoteGroove::default, |g| g.terms(n.start)),
            })
            .collect();
        notes.sort_by_key(|n| (n.start, n.key));
        Self { len: (to - from).max(0), notes }
    }
}

pub(crate) struct AudioClipData {
    pub audio: Option<Arc<DecodedAudio>>,
    /// Warped: source plays at `original_bpm` (which equals the compiled
    /// project tempo after offline pitch/time warping for pitch-preserving modes).
    pub warped: bool,
    pub original_bpm: f64,
    /// Source speed asked for by transpose and detune (`1.0` when pitch-preserving
    /// because pitch shift is pre-warped at unchanged clip duration).
    pub ratio: f64,
    pub mode: WarpMode,
    pub pitch_preserving: bool,
    pub transients: Arc<[usize]>,
    /// Read the source backwards.
    pub reverse: bool,
    /// Warped at the very tempo of the project, at the source's own speed: it
    /// plays whole frames like an unwarped clip, as long as the tempo does
    /// not move under it (see [`tempo_is_steady`]).
    pub unity: bool,
}

impl AudioClipData {
    /// How a file with `warp` plays without a known project tempo (defaults to `original_bpm`).
    #[cfg(test)]
    pub fn of(warp: &aitrack_model::Warp, audio: Option<Arc<DecodedAudio>>) -> Self {
        Self::of_bpm(warp, audio, warp.original_bpm.unwrap_or(120.0))
    }

    /// [`of_bpm_in`](Self::of_bpm_in) with stretches rendered on the spot.
    #[cfg(test)]
    pub fn of_bpm(warp: &aitrack_model::Warp, audio: Option<Arc<DecodedAudio>>, project_bpm: f64) -> Self {
        Self::of_bpm_in(warp, audio, project_bpm, &SampleCache::blocking())
    }

    /// How a file with `warp` plays against `project_bpm`. Pitch-preserving
    /// warp modes pre-render off the audio thread (with caching) at `project_bpm`
    /// and the requested transpose/detune so timeline duration stays independent
    /// of pitch and playback needs zero real-time allocations.
    ///
    /// With a background `warps` cache the render takes a while: until it is
    /// ready the clip plays the file as it is, unstretched, and the cache
    /// asks for a recompile when it lands.
    pub fn of_bpm_in(warp: &aitrack_model::Warp, audio: Option<Arc<DecodedAudio>>, project_bpm: f64, warps: &SampleCache) -> Self {
        let warped = warp.enabled && warp.original_bpm.is_some_and(|b| b > 0.0);
        let original_bpm = warp.original_bpm.unwrap_or(120.0);
        let pitch_preserving = warped && warp.mode.preserves_pitch();
        // Only the very same tempo: any difference, however small, drifts
        // when the file is read frame by frame instead of by the grid.
        let at_tempo = warped && project_bpm > 0.0 && original_bpm == project_bpm;
        if pitch_preserving
            && let Some(ref src) = audio
            && src.frames > 0
            && project_bpm > 0.0
        {
            let time_scale = (original_bpm / project_bpm).clamp(0.125, 8.0);
            let pitch_ratio = warp.pitch_ratio();
            let pitch_cents = (warp.pitch() * 100.0).round() as i32;
            if let Some((warped_audio, transients)) = warps.warped(src, warp.mode, time_scale, pitch_ratio, pitch_cents) {
                return Self {
                    audio: Some(warped_audio),
                    warped: true,
                    original_bpm: project_bpm,
                    ratio: 1.0,
                    mode: warp.mode,
                    pitch_preserving: true,
                    transients,
                    reverse: warp.reverse,
                    unity: at_tempo,
                };
            }
            // Still rendering, or the render failed: Re-Pitch, which follows
            // the grid (the pitch moves with the tempo) and needs no render.
            // The render takes over at the same position when it lands.
            return Self {
                audio,
                warped: true,
                original_bpm,
                ratio: warp.pitch_ratio(),
                mode: WarpMode::Repitch,
                pitch_preserving: false,
                transients: Arc::from([]),
                reverse: warp.reverse,
                unity: false,
            };
        }
        Self {
            audio,
            warped,
            original_bpm,
            ratio: warp.pitch_ratio(),
            mode: warp.mode,
            pitch_preserving: false,
            transients: Arc::from([]),
            reverse: warp.reverse,
            unity: at_tempo && warp.pitch_ratio() == 1.0,
        }
    }

    /// Whether the clip plays whole frames at the file's own speed.
    #[inline]
    fn exact(&self) -> bool {
        !self.warped && !self.pitch_preserving && self.ratio == 1.0
    }

    /// Frame `index` of the source as this clip reads it (mirrored when reversed).
    #[inline]
    fn frame(&self, audio: &DecodedAudio, ch: usize, index: i64) -> f32 {
        audio.frame(ch, if self.reverse { audio.frames as i64 - 1 - index } else { index })
    }

    /// Interpolated read at a fractional frame (mirrored when reversed).
    #[inline]
    fn sample(&self, audio: &DecodedAudio, ch: usize, pos: f64) -> f32 {
        audio.sample(ch, if self.reverse { (audio.frames as f64 - 1.0) - pos } else { pos })
    }

    /// Warp hook: fractional source frame for a source tick. Unwarped clips
    /// map ticks at the tempo where the clip started (`unwarped_spt` =
    /// samples per tick there); warped clips at their original tempo. A
    /// time-stretch engine replaces this mapping and renders through its own
    /// buffer instead of [`DecodedAudio::sample`].
    #[inline]
    pub fn source_frame(&self, source_tick: f64, unwarped_spt: f64) -> f64 {
        let rate = self.audio.as_ref().map_or(48_000.0, |a| a.sample_rate as f64);
        if self.warped {
            source_tick * rate * 60.0 / (self.original_bpm * PPQ as f64)
        } else {
            source_tick * unwarped_spt
        }
    }
}

/// Whether the tempo stays at `bpm` from tick `from` up to tick `to`: no
/// tempo point inside the span differs from it and no ramp crosses it. Reads
/// the point list in place, no allocation.
fn tempo_is_steady(tempo: &TempoMap, from: Tick, to: Tick, bpm: f64) -> bool {
    let tol = bpm * 1e-9;
    let points = tempo.points();
    for (i, p) in points.iter().enumerate() {
        if p.start >= to {
            break;
        }
        let next = points.get(i + 1);
        if next.is_some_and(|n| n.start <= from) {
            continue;
        }
        if (p.bpm - bpm).abs() > tol || (p.ramp && next.is_some_and(|n| (n.bpm - p.bpm).abs() > tol)) {
            return false;
        }
    }
    true
}

pub(crate) enum ClipContent {
    Midi { intro: Section, cycle: Option<Section> },
    Audio(AudioClipData),
}

/// Fades and envelopes (volume, pan, transpose) of an audio clip, copied out
/// of the model when the graph is compiled so the audio thread only reads
/// them.
///
/// [`gain`](Self::gain) is `Clip::gain_at` on a fractional clip time: equal
/// to it on whole ticks, continuous in between.
#[derive(Debug, Default)]
pub(crate) struct ClipShape {
    pub fade_in: Fade,
    pub fade_out: Fade,
    /// Points of the enabled volume envelope, sorted by tick (clip time).
    pub envelope: Box<[AutomationPoint]>,
    /// Points of the enabled pan envelope (-1..=1), sorted by tick.
    pub pan: Box<[AutomationPoint]>,
    /// Points of the enabled transpose envelope (semitones), sorted by tick.
    pub pitch: Box<[AutomationPoint]>,
    /// `∫ 2^(semitones/12) dt` from clip time 0 to `k × pitch_step`, in ticks
    /// (the speed of a Re-Pitch clip is the transpose as a ratio; this is how
    /// far into the file it has read). Empty without a transpose envelope.
    pitch_area: Box<[f64]>,
    pitch_step: f64,
}

/// Finest spacing of the transpose area table, in ticks.
const PITCH_STEP: f64 = (PPQ / 16) as f64;
/// Most cells the transpose area table gets (a very long envelope widens the step).
const PITCH_CELLS: f64 = 131_072.0;

/// Value of sorted `pts` at clip time `t` with a cursor `seg` (start it at 0
/// and pass it back, so consecutive samples do not search again). `None`
/// without points.
#[inline]
fn points_at(pts: &[AutomationPoint], t: f64, seg: &mut usize) -> Option<f32> {
    let first = pts.first()?;
    if t <= first.tick as f64 {
        return Some(first.value);
    }
    let mut i = (*seg).min(pts.len() - 1);
    while i + 1 < pts.len() && pts[i + 1].tick as f64 <= t {
        i += 1;
    }
    while i > 0 && pts[i].tick as f64 > t {
        i -= 1;
    }
    *seg = i;
    let a = pts[i];
    let Some(b) = pts.get(i + 1) else { return Some(a.value) };
    let x = (t - a.tick as f64) / b.tick.saturating_sub(a.tick).max(1) as f64;
    Some(a.value + (b.value - a.value) * curve_shape(x as f32, a.curve))
}

/// [`points_at`] without a cursor: a binary search, for random access.
#[inline]
fn points_search(pts: &[AutomationPoint], t: f64) -> Option<f32> {
    let first = pts.first()?;
    let mut seg = if t <= first.tick as f64 { 0 } else { pts.partition_point(|p| p.tick as f64 <= t).saturating_sub(1) };
    points_at(pts, t, &mut seg)
}

impl ClipShape {
    fn of(clip: &Clip) -> Self {
        // A curvature outside -1..=1 (or NaN) would put NaN in the audio.
        let curve = |c: f32| if c.is_finite() { c.clamp(-1.0, 1.0) } else { 0.0 };
        let fade = |f: Fade| Fade { length: f.length.max(0), curve: curve(f.curve) };
        let points = |kind: ClipEnv| -> Box<[AutomationPoint]> {
            let mut pts: Vec<AutomationPoint> = clip
                .envelope(kind)
                .map(|lane| lane.points.iter().map(|p| AutomationPoint { tick: p.tick, value: kind.clamp(p.value), curve: curve(p.curve) }).collect())
                .unwrap_or_default();
            pts.sort_by_key(|p| p.tick);
            pts.into_boxed_slice()
        };
        let mut shape = Self { fade_in: fade(clip.fade_in), fade_out: fade(clip.fade_out), envelope: points(ClipEnv::Volume), pan: points(ClipEnv::Pan), pitch: points(ClipEnv::Pitch), ..Self::default() };
        shape.build_pitch_area();
        shape
    }

    /// Fill the transpose area table (control thread, once per compile).
    fn build_pitch_area(&mut self) {
        let Some(last) = self.pitch.last().map(|p| p.tick.max(0) as f64) else { return };
        let step = PITCH_STEP.max((last / PITCH_CELLS).ceil());
        let cells = (last / step).ceil() as usize + 1;
        let speed = |t: f64| 2f64.powf(points_search(&self.pitch, t).unwrap_or(0.0) as f64 / 12.0);
        let mut area = Vec::with_capacity(cells + 1);
        let mut sum = 0.0;
        area.push(sum);
        for k in 0..cells {
            let (a, b) = (k as f64 * step, (k + 1) as f64 * step);
            sum += step / 6.0 * (speed(a) + 4.0 * speed((a + b) / 2.0) + speed(b));
            area.push(sum);
        }
        self.pitch_area = area.into_boxed_slice();
        self.pitch_step = step;
    }

    /// Whether the clip has any fade or envelope at all, except the transpose
    /// one ([`has_pitch`](Self::has_pitch)).
    #[inline]
    pub fn active(&self) -> bool {
        self.fade_in.length > 0 || self.fade_out.length > 0 || !self.envelope.is_empty() || !self.pan.is_empty()
    }

    #[inline]
    pub fn has_pitch(&self) -> bool {
        !self.pitch.is_empty()
    }

    /// Fades × envelope at clip time `t` (ticks since the clip started).
    /// `end` is the clip time the fade-out ends at; `None` for a clip that
    /// never ends (a looping clip in the Session View), which has no
    /// fade-out. `seg` is the caller's envelope cursor: start it at 0 and
    /// pass it back, so consecutive samples do not search the points again.
    #[inline]
    pub fn gain(&self, t: f64, end: Option<Tick>, seg: &mut usize) -> f32 {
        let mut gain = 1.0f32;
        let fade_in = self.fade_in.length as f64;
        if fade_in > 0.0 && t < fade_in {
            gain *= curve_shape((t.max(0.0) / fade_in) as f32, self.fade_in.curve);
        }
        let fade_out = self.fade_out.length as f64;
        if fade_out > 0.0
            && let Some(end) = end
        {
            let left = end as f64 - t;
            if left < fade_out {
                gain *= curve_shape((left.max(0.0) / fade_out) as f32, self.fade_out.curve);
            }
        }
        if let Some(v) = points_at(&self.envelope, t, seg) {
            gain *= v.clamp(0.0, 1.0);
        }
        gain
    }

    /// Left and right gain of the pan envelope at clip time `t` (unity without
    /// one). `seg` is a cursor as for [`gain`](Self::gain).
    #[inline]
    pub fn pan_gains(&self, t: f64, seg: &mut usize) -> (f32, f32) {
        match points_at(&self.pan, t, seg) {
            Some(v) => clip_pan_gains(v),
            None => (1.0, 1.0),
        }
    }

    /// Transpose in semitones at clip time `t`.
    #[inline]
    pub fn semitones(&self, t: f64) -> f64 {
        points_search(&self.pitch, t).map_or(0.0, f64::from)
    }

    /// `∫ 2^(transpose/12) dτ` for τ in `0..t`: the clip ticks of the file
    /// read after `t` ticks of clip time at the envelope's speed (plain `t`
    /// without a transpose envelope). No allocation: a table lookup and a
    /// trapezoid inside one cell.
    #[inline]
    pub fn pitch_integral(&self, t: f64) -> f64 {
        let speed = |t: f64| 2f64.powf(self.semitones(t) / 12.0);
        let Some(&end) = self.pitch_area.last() else { return t };
        if t <= 0.0 {
            return t * speed(0.0);
        }
        let last = (self.pitch_area.len() - 1) as f64;
        let k = (t / self.pitch_step).floor().min(last);
        let a = k * self.pitch_step;
        let base = if k >= last { end } else { self.pitch_area[k as usize] };
        base + (t - a) * 0.5 * (speed(a) + speed(t))
    }
}

/// Length of the anti-click ramp at a clip edge that has no fade, in samples.
const EDGE_RAMP: f64 = 64.0;
/// Length of the crossfade at a loop seam; at most half the loop.
const SEAM_SECONDS: f64 = 0.005;

/// One pass of an unwarped clip over its source: the intro (offset to loop
/// end) or one repeat of the loop.
struct Pass {
    /// Source frame played at the sample the pass was asked for.
    frame: i64,
    /// Samples since the clip started at which the pass ends.
    end: i64,
    /// Samples before `end` that crossfade into the lead-up of the loop start.
    seam: i64,
}

/// The loop of an unwarped clip in whole source frames. Every repeat starts
/// on the sample nearest to its exact position (`intro + c × span`), so a
/// loop whose length is not a whole number of samples does not drift.
struct FrameLoop {
    offset: i64,
    start: i64,
    intro: i64,
    span: f64,
    seam: i64,
}

impl FrameLoop {
    /// `offset`, `start` and `end` in source ticks, `spt` samples per tick.
    fn new(offset: Tick, start: Tick, end: Tick, spt: f64, sr: f64) -> Self {
        let frame = |tick: Tick| (tick as f64 * spt).round() as i64;
        let span = ((end - start) as f64 * spt).max(1.0);
        Self {
            offset: frame(offset),
            start: frame(start),
            intro: frame(end).saturating_sub(frame(offset)).max(0),
            span,
            seam: ((SEAM_SECONDS * sr).round() as i64).min((span / 2.0) as i64).max(0),
        }
    }

    #[inline]
    fn repeat_start(&self, c: i64) -> i64 {
        (c as f64 * self.span).round() as i64
    }

    /// The pass that plays `since` samples after the clip started.
    #[inline]
    fn pass(&self, since: i64) -> Pass {
        if since < self.intro {
            return Pass { frame: self.offset.saturating_add(since), end: self.intro, seam: self.seam.min(self.intro) };
        }
        let u = since - self.intro;
        let mut c = (u as f64 / self.span).floor() as i64;
        while c > 0 && self.repeat_start(c) > u {
            c -= 1;
        }
        while self.repeat_start(c + 1) <= u {
            c += 1;
        }
        Pass {
            frame: self.start.saturating_add(u - self.repeat_start(c)),
            end: (self.intro + self.repeat_start(c + 1)).max(since + 1),
            seam: self.seam,
        }
    }
}

pub(crate) struct CompiledClip {
    pub length: Tick,
    pub offset: Tick,
    pub looping: Option<(Tick, Tick)>,
    pub gain: f32,
    pub muted: bool,
    pub quantize: Quantize,
    pub mode: LaunchMode,
    pub follow: Option<FollowAction>,
    pub content: ClipContent,
    /// Fades and volume envelope (audio clips; empty for MIDI).
    pub shape: ClipShape,
    /// The groove the notes are played with, if the clip has one.
    pub groove: Option<ClipGroove>,
}

/// A clip's groove while it plays: the pool groove's grid and the live
/// settings and amount. The notes carry their own terms ([`SNote::groove`]);
/// everything else is applied when they are scheduled, so changing the
/// amount or a setting is a few stores, not a compile.
///
/// A change does not move notes that are already on their way: it applies
/// from `switch_tick` on (a little ahead of the playhead), to the notes whose
/// *nominal* position is at or after it, and the old values go on for the
/// ones before. A note's moment is then always a function of its position
/// and of one of two parameter sets, so it never plays twice or not at all
/// when a slider moves.
#[derive(Clone, Debug)]
pub(crate) struct ClipGroove {
    pub id: GrooveId,
    pub base: Tick,
    pub seed: u64,
    /// The farthest a note can be moved from its nominal place, in ticks.
    pub margin: Tick,
    pub settings: GrooveSettings,
    pub amount: f32,
    cur: GrooveParams,
    prev: GrooveParams,
    switch_tick: Tick,
}

impl ClipGroove {
    pub fn new(entry: &PoolGroove, amount: f32) -> Self {
        let params = GrooveParams::new(&entry.settings, amount);
        let base = entry.groove.base.max(1);
        Self {
            id: entry.id,
            base,
            seed: entry.id.0,
            // pull ≤ ½ cell, timing ≤ 1.3 × 1.3 cells, random ≤ ½ × 1.3 cells, plus rounding.
            margin: 3 * base + 2,
            settings: entry.settings,
            amount: params.amount,
            cur: params,
            prev: params,
            switch_tick: Tick::MIN,
        }
    }

    /// The parameters for a note whose nominal place on the timeline is `tick`.
    #[inline]
    pub fn params_at(&self, tick: Tick) -> &GrooveParams {
        if tick >= self.switch_tick { &self.cur } else { &self.prev }
    }

    fn retune(&mut self, now: Tick) {
        let next = GrooveParams::new(&self.settings, self.amount);
        if next == self.cur {
            return;
        }
        if now >= self.switch_tick {
            self.prev = self.cur;
        }
        self.cur = next;
        // Past every note that may already have been scheduled: a block's
        // worth of ticks and the farthest a note moves.
        self.switch_tick = now + self.margin + PPQ / 2 + 1;
    }

    pub fn set_amount(&mut self, amount: f32, now: Tick) {
        self.amount = amount.clamp(0.0, MAX_GROOVE_AMOUNT);
        self.retune(now);
    }

    pub fn set_settings(&mut self, settings: GrooveSettings, now: Tick) {
        self.settings = settings.normalized();
        self.retune(now);
    }

    /// Every note takes the current values (the transport jumped).
    pub fn settle(&mut self) {
        self.prev = self.cur;
        self.switch_tick = Tick::MIN;
    }
}

impl CompiledClip {
    #[cfg(test)]
    pub fn midi(id: ClipId, clip: &Clip, notes: &NoteSource) -> Self {
        Self::midi_grooved(id, clip, notes, None)
    }

    /// A MIDI clip played with `groove` (the pool entry and the project's
    /// global amount), when it has one.
    pub fn midi_grooved(id: ClipId, clip: &Clip, notes: &NoteSource, groove: Option<(&PoolGroove, f32)>) -> Self {
        let g = groove.map(|(e, _)| {
            let mut g = e.groove.clone();
            g.normalize();
            g
        });
        let g = g.as_ref();
        let looping = clip.looping.filter(|l| l.end > l.start).map(|l| (l.start, l.end));
        let (intro, cycle) = match looping {
            Some((ls, le)) => {
                let intro = if clip.offset < le { Section::from_notes(notes, clip.offset, le, g) } else { Section::default() };
                (intro, Some(Section::from_notes(notes, ls, le, g)))
            }
            None => (Section::from_notes(notes, clip.offset, clip.offset + clip.length.max(0), g), None),
        };
        let mut c = Self::base(id, clip, looping, ClipContent::Midi { intro, cycle });
        c.groove = groove.map(|(e, amount)| ClipGroove::new(e, amount));
        c
    }

    pub fn audio(id: ClipId, clip: &Clip, data: AudioClipData) -> Self {
        let looping = clip.looping.filter(|l| l.end > l.start).map(|l| (l.start, l.end));
        Self { shape: ClipShape::of(clip), ..Self::base(id, clip, looping, ClipContent::Audio(data)) }
    }

    fn base(id: ClipId, clip: &Clip, looping: Option<(Tick, Tick)>, content: ClipContent) -> Self {
        let _ = id;
        Self {
            length: clip.length.max(0),
            offset: clip.offset,
            looping,
            gain: aitrack_dsp::db_to_gain(clip.gain_db),
            muted: clip.muted,
            quantize: clip.launch.quantize,
            mode: clip.launch.mode,
            follow: clip.launch.follow,
            content,
            shape: ClipShape::default(),
            groove: None,
        }
    }

    /// Length of one pass in the Session View: the loop region when looping,
    /// else the clip length.
    pub fn session_cycle(&self) -> Tick {
        match self.looping {
            Some((s, e)) => e - s,
            None => self.length,
        }
        .max(1)
    }

    /// Schedule note-ons whose sample falls in `[lo, hi)`; the clip instance
    /// starts at `start` (timeline tick) and is cut at clip time `limit`.
    #[allow(clippy::too_many_arguments)]
    pub fn schedule_midi(
        &self,
        start: Tick,
        limit: Option<Tick>,
        lo: i64,
        hi: i64,
        ctx: &TimeCtx<'_>,
        out: &mut EventList,
        active: &mut ActiveNotes,
        rng: &mut Rng,
    ) {
        let ClipContent::Midi { intro, cycle } = &self.content else { return };
        if self.muted || hi <= lo {
            return;
        }
        let wa = ctx.tick_of(lo).floor() as Tick - 1 - start;
        let wb = ctx.tick_of(hi).ceil() as Tick + 1 - start;
        // A groove can move a note away from its nominal place: look that far
        // on both sides of the block.
        let reach = self.groove.as_ref().map_or(0, |g| g.margin);
        let groove = self.groove.as_ref();
        let mut emit = |sec_start: Tick, sec: &Section| {
            let from = wa - sec_start - reach;
            let to = wb - sec_start + reach;
            let i0 = sec.notes.partition_point(|n| n.start < from);
            for n in &sec.notes[i0..] {
                if n.start >= to {
                    break;
                }
                let on = sec_start + n.start;
                if on < 0 {
                    continue;
                }
                // Where it really starts, how loud, and where it ends (ticks).
                let (begin, velocity, off_f) = match groove {
                    None => {
                        if limit.is_some_and(|l| on >= l) {
                            continue;
                        }
                        let mut off = sec_start + n.end;
                        if let Some(l) = limit {
                            off = off.min(l);
                        }
                        (on as f64, n.velocity, off as f64)
                    }
                    Some(g) => {
                        let params = g.params_at(start + on);
                        let rand = if params.random > 0.0 { random_unit(g.seed, on, n.key) } else { 0.0 };
                        // The section of the clip the note belongs to: it stays inside it.
                        let (lo_t, hi_t) = (sec_start as f64, (sec_start + sec.len) as f64);
                        let b = (on as f64 + params.shift(&n.groove, g.base, rand)).clamp(lo_t, (hi_t - 1.0).max(lo_t));
                        if limit.is_some_and(|l| b >= l as f64) {
                            continue;
                        }
                        let len = (n.end - n.start) as f64;
                        let wanted = if len > 0.0 { (len + params.length_delta(&n.groove)).max(1.0) } else { 0.0 };
                        let mut off = (b + wanted).min(hi_t.max(b));
                        if let Some(l) = limit {
                            off = off.min(l as f64);
                        }
                        (b, grooved_velocity(n.velocity, params.velocity_factor(&n.groove)), off)
                    }
                };
                let at = ctx.sample_of_f(start as f64 + begin);
                if at < lo || at >= hi {
                    continue;
                }
                if n.probability < 1.0 && rng.next_f32() >= n.probability {
                    continue;
                }
                let offset = (at - ctx.seg_start) as u32;
                let events = 1 + usize::from(n.fine_pitch != 0.0) + usize::from(n.pan != 0.0);
                let note = ActiveNote { key: n.key, note_id: n.note_id, off_tick: start as f64 + off_f, min_off: at + 1, release: n.release };
                if !active.start(note, out, offset, events) {
                    continue;
                }
                out.push(offset, EventKind::NoteOn { channel: 0, key: n.key, velocity, note_id: n.note_id });
                if n.fine_pitch != 0.0 {
                    out.push(
                        offset,
                        EventKind::NoteExpression { note_id: n.note_id, key: n.key, expr: Expression::Tuning, value: n.fine_pitch / 100.0 },
                    );
                }
                if n.pan != 0.0 {
                    out.push(offset, EventKind::NoteExpression { note_id: n.note_id, key: n.key, expr: Expression::Pan, value: n.pan });
                }
            }
        };
        if wa < intro.len && wb > 0 {
            emit(0, intro);
        }
        if let Some(cyc) = cycle.as_ref().filter(|c| c.len > 0) {
            let base = intro.len;
            if wb > base {
                let k0 = ((wa - base).max(0)) / cyc.len;
                let k1 = (wb - 1 - base) / cyc.len;
                for k in k0..=k1 {
                    emit(base + k * cyc.len, cyc);
                }
            }
        }
    }

    /// Whether the end of the clip falls inside the material it plays, so that
    /// the cut needs the anti-click ramp. Looping clips are always cut; a clip
    /// that never ends (`limit` `None`) has no end to ramp. Only a clip that
    /// reaches the last sample of the file (within one sample) is not cut.
    #[allow(clippy::too_many_arguments)]
    fn cuts_tail(&self, data: &AudioClipData, audio: &DecodedAudio, offset: Tick, looping: bool, limit: Option<Tick>, spt: f64, reach: f64) -> bool {
        let Some(_) = limit else { return false };
        if looping {
            return true;
        }
        let end = data.source_frame(offset as f64 + reach, spt);
        end < audio.frames as f64 - 1.0
    }

    /// Mix the audio of this clip instance for samples `[lo, hi)` into `buf`
    /// (whose sample 0 is `ctx.seg_start`). The instance starts at `start`
    /// (timeline tick) and ends at clip time `limit`; `None` plays on forever
    /// (a looping clip in the Session View).
    ///
    /// Gain is the clip gain × fades × volume envelope at the clip time of
    /// each sample (timeline ticks since `start`), and the pan envelope turns
    /// one channel down. The transpose envelope changes the speed of a
    /// Re-Pitch clip (the position in the file is the integral of the speed)
    /// and shifts the pitch of a pitch-preserving one without moving it in
    /// time ([`pitch_taps`]). An edge without a fade
    /// keeps its 64-sample anti-click ramp: at the end when the clip cuts the
    /// material off, and at the start when the clip is trimmed (a clip that
    /// starts at the very beginning of its file keeps the transient the file
    /// starts with).
    #[allow(clippy::too_many_arguments)]
    pub fn render_audio(&self, start: Tick, limit: Option<Tick>, lo: i64, hi: i64, ctx: &TimeCtx<'_>, buf: &mut AudioBuffer) {
        let ClipContent::Audio(data) = &self.content else { return };
        let Some(audio) = &data.audio else { return };
        if self.muted || hi <= lo || buf.channels() < 2 || audio.frames == 0 {
            return;
        }
        let start_exact = ctx.tempo.tick_to_samples(start, ctx.sr);
        let spt = ctx.sr * 60.0 / (ctx.tempo.bpm_at(start) * PPQ as f64);
        let end_sample = limit.map(|l| ctx.tempo.tick_to_samples(start + l, ctx.sr));
        let stereo = audio.channels.len() >= 2;
        let shape = &self.shape;
        let shaped = shape.active();
        // A clip that starts past its loop end starts inside the loop, at the
        // position playback would have wrapped to.
        let looping = self.looping.filter(|(ls, le)| le > ls);
        let offset = match looping {
            Some((ls, le)) if self.offset >= le => ls + self.offset.saturating_sub(ls).rem_euclid(le - ls),
            _ => self.offset,
        };
        // The end ramps down only where the clip cuts the material off: a clip
        // that plays its file to the end (or is followed by the rest of it)
        // would otherwise leave a notch at every join of contiguous clips.
        // A transpose envelope moves a Re-Pitch clip through its file at a
        // varying speed; a pitch-preserving clip stays on its time grid.
        let repitch_env = shape.has_pitch() && !data.pitch_preserving;
        let shift_env = shape.has_pitch() && data.pitch_preserving;
        let reach = limit.map_or(0.0, |l| if repitch_env { data.ratio * shape.pitch_integral(l as f64) } else { l as f64 * data.ratio });
        let ramp_out = shape.fade_out.length == 0 && self.cuts_tail(data, audio, offset, looping.is_some(), limit, spt, reach);
        let ramp_in = self.offset != 0 && shape.fade_in.length == 0;
        // Clip gain with the anti-click ramps, `since` samples into the clip.
        let edge_gain = |k: i64, since: f64| -> f32 {
            let mut g = self.gain;
            if ramp_in && since < EDGE_RAMP {
                g *= (since / EDGE_RAMP).max(0.0) as f32;
            }
            if ramp_out
                && let Some(e) = end_sample
            {
                let until = e - k as f64;
                if until < EDGE_RAMP {
                    g *= (until / EDGE_RAMP).max(0.0) as f32;
                }
            }
            g
        };
        let mut seg = 0usize;
        let mut pan_seg = 0usize;
        let panned = !shape.pan.is_empty();
        let (l, r) = buf.stereo_mut();

        // A clip at its native speed reads whole frames: playback is
        // bit-identical to the file instead of an interpolated copy of it.
        let whole_frames = !shape.has_pitch() && (data.exact() || (data.unity && tempo_is_steady(ctx.tempo, start, limit.map_or(Tick::MAX, |l| start.saturating_add(l)), data.original_bpm)));
        if whole_frames {
            // The first sample at or after the clip start, as the caller's
            // range counts it: rounding down would put frame 0 before it.
            let start_i = ctx.sample_of(start);
            let stop = end_sample.map_or(hi, |e| hi.min(e.ceil() as i64));
            let frames = looping.map(|(ls, le)| FrameLoop::new(offset, ls, le, spt, ctx.sr));
            let loop_start = frames.as_ref().map_or(0, |f| f.start);
            // A loop that starts at the beginning of the file has nothing
            // leading up to it: the tail fades into its first frame instead.
            let lead_floor = loop_start.min(0);
            let first_frame = (offset as f64 * spt).round() as i64;
            let mut k = lo.max(start_i);
            while k < stop {
                let since = k - start_i;
                let pass = match &frames {
                    Some(f) => f.pass(since),
                    None => Pass { frame: first_frame.saturating_add(since), end: i64::MAX, seam: 0 },
                };
                let pass_end = start_i.saturating_add(pass.end);
                let run_end = stop.min(pass_end);
                let mut n = k;
                let mut t0 = if shaped { ctx.tick_of(n) - start as f64 } else { 0.0 };
                while n < run_end {
                    let chunk_end = (n + crate::automation::CONTROL_BLOCK as i64).min(run_end);
                    let t1 = if shaped { ctx.tick_of(chunk_end) - start as f64 } else { 0.0 };
                    let dt = (t1 - t0) / (chunk_end - n) as f64;
                    for j in n..chunk_end {
                        let frame = pass.frame.saturating_add(j - k);
                        let mut sl = data.frame(audio, 0, frame);
                        let mut sr = if stereo { data.frame(audio, 1, frame) } else { sl };
                        if pass.seam > 0 {
                            let left = pass_end - j;
                            if left <= pass.seam {
                                // Fade the tail into what leads up to the loop start.
                                let w = (pass.seam - left + 1) as f32 / (pass.seam + 1) as f32;
                                let lead = (loop_start - left).max(lead_floor);
                                let pl = data.frame(audio, 0, lead);
                                let pr = if stereo { data.frame(audio, 1, lead) } else { pl };
                                sl += (pl - sl) * w;
                                sr += (pr - sr) * w;
                            }
                        }
                        let mut g = edge_gain(j, (j - start_i) as f64);
                        let (mut gl, mut gr) = (1.0f32, 1.0f32);
                        if shaped {
                            let t = t0 + dt * (j - n) as f64;
                            g *= shape.gain(t, limit, &mut seg);
                            if panned {
                                (gl, gr) = shape.pan_gains(t, &mut pan_seg);
                            }
                        }
                        let idx = (j - ctx.seg_start) as usize;
                        l[idx] += sl * (g * gl);
                        r[idx] += sr * (g * gr);
                    }
                    t0 = t1;
                    n = chunk_end;
                }
                k = run_end;
            }
            return;
        }

        // Repitch (warped, transposed or detuned): the source position follows
        // the tempo map and is read through the interpolator.
        let ratio = data.ratio;
        let span = looping.map_or(0.0, |(ls, le)| (le - ls) as f64);
        // Seam length in source ticks: for a repeat, and for the first pass
        // (which cannot start crossfading before the clip does).
        let (seam, seam_intro) = match looping {
            Some((_, le)) => {
                let ticks = (SEAM_SECONDS * data.original_bpm * PPQ as f64 / 60.0).min(span / 2.0);
                (ticks, ticks.min((le - offset) as f64))
            }
            None => (0.0, 0.0),
        };
        // As above: without a lead-up the tail fades into the first frame.
        let lead_floor = looping.map_or(0.0, |(ls, _)| data.source_frame(ls as f64, spt).min(0.0));
        // If a pitch-preserving clip is crossing a dynamic tempo ramp or live BPM
        // change away from its compiled tempo, use the allocation-free RT stretch reader.
        let tempo_steady = !data.pitch_preserving
            || ((ctx.tempo.bpm_at(start) - data.original_bpm).abs() <= data.original_bpm * 0.005
                && (ctx.tempo.bpm_at(start + limit.unwrap_or(PPQ)) - data.original_bpm).abs() <= data.original_bpm * 0.005);
        let ch0 = &audio.channels[0];
        let ch1 = if stereo { &audio.channels[1] } else { ch0 };
        let sr = audio.sample_rate as f64;
        // Source ticks read `t` clip ticks in (beyond `offset`).
        let reads = |t: f64| if repitch_env { ratio * shape.pitch_integral(t) } else { t * ratio };
        // Frames the pitch shift of a pitch-preserving clip has run ahead.
        let frames_per_tick = data.source_frame(1.0, spt);
        let shift_window = sr * 0.04;
        let mut n = lo;
        while n < hi {
            let chunk_end = (n + crate::automation::CONTROL_BLOCK as i64).min(hi);
            let t0 = ctx.tick_of(n) - start as f64;
            let t1 = ctx.tick_of(chunk_end) - start as f64;
            let len = (chunk_end - n) as f64;
            let frame_0 = data.source_frame(offset as f64 + reads(t0), spt);
            let frame_1 = data.source_frame(offset as f64 + reads(t1), spt);
            let local_step = (frame_1 - frame_0) / len;
            for k in n..chunk_end {
                let tau = t0 + (t1 - t0) * (k - n) as f64 / len;
                if tau < 0.0 || limit.is_some_and(|lim| tau >= lim as f64) {
                    continue;
                }
                let mut src = offset as f64 + reads(tau);
                let mut w = 0.0f32;
                if let Some((ls, le)) = looping {
                    let mut fade = seam_intro;
                    if src >= le as f64 {
                        src = ls as f64 + (src - le as f64).rem_euclid(span);
                        fade = seam;
                    }
                    let left = le as f64 - src;
                    if left < fade {
                        w = (1.0 - left / fade) as f32;
                    }
                }
                let frame = data.source_frame(src, spt);
                // The clip read at file frame `at`, both channels.
                let read = |at: f64| -> (f32, f32) {
                    if tempo_steady {
                        (data.sample(audio, 0, at), if stereo { data.sample(audio, 1, at) } else { 0.0 })
                    } else if data.mode == WarpMode::Beats {
                        let s0 = sample_warped_beats(ch0, &data.transients, at, sr, data.reverse);
                        let s1 = if stereo { sample_warped_beats(ch1, &data.transients, at, sr, data.reverse) } else { s0 };
                        (s0, s1)
                    } else {
                        let since = k as f64 - start_exact;
                        let jitter = data.mode == WarpMode::Texture;
                        let src_at_hop = |center_out: f64| at + (center_out - since) * local_step;
                        let s0 = sample_warped_grain(ch0, 0x1234_5678, since, src_at_hop, sr, jitter, data.reverse);
                        let s1 = if stereo { sample_warped_grain(ch1, 0x8765_4321, since, src_at_hop, sr, jitter, data.reverse) } else { s0 };
                        (s0, s1)
                    }
                };
                let (mut sl, mut sr_s) = if shift_env {
                    // Two taps that wander around the aligned position at the
                    // speed of the shift (see `pitch_taps`).
                    let drift = (shape.pitch_integral(tau) - tau) * ratio * frames_per_tick;
                    let [(oa, wa), (ob, wb)] = pitch_taps(drift, shift_window);
                    let (a0, a1) = read(frame + oa);
                    let (b0, b1) = read(frame + ob);
                    (a0 * wa + b0 * wb, a1 * wa + b1 * wb)
                } else {
                    read(frame)
                };
                if !stereo && tempo_steady {
                    sr_s = sl;
                }
                if w > 0.0 {
                    let lead = data.source_frame(src - span, spt).max(lead_floor);
                    let pl = data.sample(audio, 0, lead);
                    let pr = if stereo { data.sample(audio, 1, lead) } else { pl };
                    sl += (pl - sl) * w;
                    sr_s += (pr - sr_s) * w;
                }
                let mut g = edge_gain(k, k as f64 - start_exact);
                let (mut gl, mut gr) = (1.0f32, 1.0f32);
                if shaped {
                    g *= shape.gain(tau, limit, &mut seg);
                    if panned {
                        (gl, gr) = shape.pan_gains(tau, &mut pan_seg);
                    }
                }
                let idx = (k - ctx.seg_start) as usize;
                l[idx] += sl * (g * gl);
                r[idx] += sr_s * (g * gr);
            }
            n = chunk_end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample_cache::SampleCache;
    use aitrack_model::{Color, LoopRegion, Note, NoteId, SourceId};

    fn clip(offset: Tick, length: Tick, looping: Option<(Tick, Tick)>) -> Clip {
        Clip { offset, looping: looping.map(|(start, end)| LoopRegion { start, end }), ..Clip::new("c", Color(0), SourceId(1), length) }
    }

    fn notes() -> NoteSource {
        NoteSource { notes: (0..4).map(|i| Note::new(NoteId(i as u64 + 10), 60 + i, i as Tick * PPQ, PPQ / 2, 1.0)).collect() }
    }

    fn collect(c: &CompiledClip, start: Tick, limit: Option<Tick>, total: i64) -> Vec<(i64, u8)> {
        let tempo = TempoMap::constant(120.0);
        let mut out = Vec::new();
        let mut active = ActiveNotes::new();
        let mut rng = Rng(1);
        let block = 100;
        let mut s = 0;
        while s < total {
            let ctx = TimeCtx { tempo: &tempo, sr: 960.0, seg_start: s };
            let mut ev = EventList::with_capacity(64);
            c.schedule_midi(start, limit, s, s + block, &ctx, &mut ev, &mut active, &mut rng);
            for e in &ev.events {
                if let EventKind::NoteOn { key, .. } = e.kind {
                    out.push((s + e.offset as i64, key));
                }
            }
            s += block;
        }
        out
    }

    #[test]
    fn plain_clip_exactly_once() {
        // sr 960 at 120 BPM → 480 samples per beat.
        let c = CompiledClip::midi(ClipId(1), &clip(0, 4 * PPQ, None), &notes());
        let ons = collect(&c, 0, Some(4 * PPQ), 4000);
        assert_eq!(ons, vec![(0, 60), (480, 61), (960, 62), (1440, 63)]);
    }

    #[test]
    fn looping_with_offset() {
        // Offset 1 beat into a 2-beat loop [0, 2): intro plays note 61, then loops 60, 61.
        let c = CompiledClip::midi(ClipId(1), &clip(PPQ, 5 * PPQ, Some((0, 2 * PPQ))), &notes());
        let ons = collect(&c, 0, Some(5 * PPQ), 4000);
        let keys: Vec<u8> = ons.iter().map(|o| o.1).collect();
        assert_eq!(keys, vec![61, 60, 61, 60, 61]);
        assert_eq!(ons[1].0, 480);
    }

    /// Run a clip block by block the way the track does (events cleared, clip
    /// scheduled, due releases) and list the note ids that sounded and the
    /// ones that were released. `cap` is the event list capacity.
    fn run_notes(c: &CompiledClip, limit: Tick, cap: usize, blocks: i64) -> (Vec<i32>, Vec<i32>, ActiveNotes, u32) {
        let tempo = TempoMap::constant(120.0);
        let (mut ons, mut offs) = (Vec::new(), Vec::new());
        let mut active = ActiveNotes::new();
        let mut rng = Rng(1);
        let mut dropped = 0;
        for b in 0..blocks {
            let (lo, hi) = (b * 100, b * 100 + 100);
            let ctx = TimeCtx { tempo: &tempo, sr: 960.0, seg_start: lo };
            let mut ev = EventList::with_capacity(cap);
            c.schedule_midi(0, Some(limit), lo, hi, &ctx, &mut ev, &mut active, &mut rng);
            active.release_due(&ctx, hi, &mut ev);
            dropped += ev.dropped;
            for e in &ev.events {
                match e.kind {
                    EventKind::NoteOn { note_id, .. } => ons.push(note_id),
                    EventKind::NoteOff { note_id, .. } => offs.push(note_id),
                    _ => {}
                }
            }
        }
        (ons, offs, active, dropped)
    }

    /// More simultaneous notes than the event list holds: what sounds must
    /// stop. A note-on is only emitted when its note-off is certain to fit.
    #[test]
    fn every_note_on_gets_its_note_off_when_the_event_list_is_short() {
        let many = NoteSource { notes: (0..3000).map(|i| Note::new(NoteId(i as u64 + 1), (i % 128) as u8, 0, PPQ / 4, 1.0)).collect() };
        let c = CompiledClip::midi(ClipId(1), &clip(0, PPQ, None), &many);
        let (ons, offs, active, dropped) = run_notes(&c, PPQ, 2048, 20);
        assert!(!ons.is_empty());
        assert!(active.notes.is_empty(), "{} notes still held", active.notes.len());
        let mut hung: Vec<i32> = ons.iter().copied().filter(|id| ons.iter().filter(|o| *o == id).count() != offs.iter().filter(|o| *o == id).count()).collect();
        hung.dedup();
        assert!(hung.is_empty(), "{} of {} note-ons never stopped", hung.len(), ons.len());
        // The notes that did not fit were refused, and counted as such.
        assert_eq!(ons.len() + dropped as usize, 3000, "refused notes are counted");
        // A roomy list plays all of the notes that fit the keyboard.
        let (ons, ..) = run_notes(&c, PPQ, 16_384, 20);
        assert_eq!(ons.len(), 3000);
    }

    /// A note-on that the active list cannot hold would never be released.
    #[test]
    fn a_note_the_active_list_cannot_hold_is_not_started() {
        let mut active = ActiveNotes::with_capacity(4);
        let cap = active.notes.capacity();
        let mut out = EventList::with_capacity(64);
        for key in 0..(cap as u8 + 3) {
            let note = ActiveNote { key, note_id: key as i32, off_tick: PPQ as f64, min_off: 1, release: 0.0 };
            if active.start(note, &mut out, 0, 1) {
                out.push(0, EventKind::NoteOn { channel: 0, key, velocity: 1.0, note_id: key as i32 });
            }
        }
        let started = out.events.iter().filter(|e| matches!(e.kind, EventKind::NoteOn { .. })).count();
        assert_eq!(started, cap);
        assert_eq!(active.notes.len(), cap);
    }

    // ───────────────────────────── Audio ─────────────────────────────

    use aitrack_model::{AutomationLane, AutomationTarget};

    fn audio_clip(clip: &Clip, data: Vec<f32>, original_bpm: Option<f64>) -> CompiledClip {
        let audio = Arc::new(DecodedAudio::from_planar(vec![data], 48_000));
        let data = AudioClipData {
            audio: Some(audio),
            warped: original_bpm.is_some(),
            original_bpm: original_bpm.unwrap_or(120.0),
            ratio: 1.0,
            mode: WarpMode::Repitch,
            pitch_preserving: false,
            transients: Arc::from([]),
            reverse: false,
            unity: false,
        };
        CompiledClip::audio(ClipId(1), clip, data)
    }

    /// The first `total` samples of the clip placed at `start`, at 48 kHz.
    fn play(c: &CompiledClip, bpm: f64, start: Tick, limit: Option<Tick>, total: usize) -> Vec<f32> {
        let tempo = TempoMap::constant(bpm);
        let mut out = Vec::with_capacity(total);
        let mut buf = AudioBuffer::new(2, 100);
        let mut s = 0i64;
        while (s as usize) < total {
            buf.clear();
            let ctx = TimeCtx { tempo: &tempo, sr: 48_000.0, seg_start: s };
            c.render_audio(start, limit, s, s + 100, &ctx, &mut buf);
            out.extend_from_slice(buf.channel(0));
            s += 100;
        }
        out
    }

    fn pitched_clip(clip: &Clip, data: Vec<f32>, ratio: f64, reverse: bool) -> CompiledClip {
        let audio = Arc::new(DecodedAudio::from_planar(vec![data], 48_000));
        CompiledClip::audio(
            ClipId(1),
            clip,
            AudioClipData {
                audio: Some(audio),
                warped: false,
                original_bpm: 120.0,
                ratio,
                mode: WarpMode::Repitch,
                pitch_preserving: false,
                transients: Arc::from([]),
                reverse,
                unity: false,
            },
        )
    }

    #[test]
    fn pitch_preserving_warp_modes_keep_duration_and_shift_pitch() {
        use aitrack_model::Warp;
        let len = 48_000usize;
        let sine: Vec<f32> = (0..len).map(|i| 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin()).collect();
        let audio = Arc::new(DecodedAudio::from_planar(vec![sine], 48_000));
        let c = clip(0, 2 * PPQ, None);
        for mode in [WarpMode::Beats, WarpMode::Tones, WarpMode::Texture, WarpMode::Complex, WarpMode::ComplexPro] {
            let warp = Warp { enabled: true, mode, original_bpm: Some(120.0), transpose: 12.0, ..Warp::default() };
            let compiled = CompiledClip::audio(ClipId(1), &c, AudioClipData::of_bpm(&warp, Some(audio.clone()), 120.0));
            let out = play(&compiled, 120.0, 0, Some(2 * PPQ), 48_000);
            // Clip still fills all 1.0s (48_000 samples) instead of finishing at 0.5s like Re-Pitch.
            let tail_energy: f32 = out[30_000..44_000].iter().map(|s| s * s).sum();
            assert!(tail_energy > 1.0, "{mode:?} must keep clip duration when transposed");
            // Frequency doubled from 440 Hz to ~880 Hz.
            let seg = &out[8_000..16_000];
            let crossings = seg.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
            let hz = crossings as f32 * 48_000.0 / seg.len() as f32;
            assert!((hz - 880.0).abs() < 45.0, "{mode:?} +12st expected ~880 Hz, got {hz}");
        }
    }

    #[test]
    fn transpose_repitches_an_unwarped_clip() {
        // A ramp: the value read tells where in the file playback is.
        let ramp: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.).collect();
        let c = clip(0, 8 * PPQ, None);
        // +12 st: twice as fast, so 1000 samples in it reads frame 2000.
        let up = play(&pitched_clip(&c, ramp.clone(), 2f64.powf(12. / 12.), false), 120., 0, Some(8 * PPQ), 1200);
        assert!((up[1000] - 2000. / 48_000.).abs() < 1e-3, "{}", up[1000]);
        // -12 st: half speed.
        let down = play(&pitched_clip(&c, ramp.clone(), 0.5, false), 120., 0, Some(8 * PPQ), 1200);
        assert!((down[1000] - 500. / 48_000.).abs() < 1e-3, "{}", down[1000]);
        // +50 ct: 2^(0.5/12) faster.
        let fine = play(&pitched_clip(&c, ramp, 2f64.powf(0.5 / 12.), false), 120., 0, Some(8 * PPQ), 1200);
        assert!((fine[1000] - 1000. * 2f32.powf(0.5 / 12.) / 48_000.).abs() < 1e-3, "{}", fine[1000]);
    }

    fn lane(target: AutomationTarget, points: &[(Tick, f32)]) -> AutomationLane {
        let mut lane = AutomationLane::new(target);
        for &(tick, value) in points {
            lane.insert(AutomationPoint { tick, value, curve: 0.0 });
        }
        lane
    }

    /// Both channels of the first `total` samples (at 48 kHz, 120 bpm unless said).
    fn play_stereo(c: &CompiledClip, start: Tick, limit: Option<Tick>, total: usize) -> (Vec<f32>, Vec<f32>) {
        let tempo = TempoMap::constant(120.0);
        let (mut l, mut r) = (Vec::with_capacity(total), Vec::with_capacity(total));
        let mut buf = AudioBuffer::new(2, 100);
        let mut s = 0i64;
        while (s as usize) < total {
            buf.clear();
            let ctx = TimeCtx { tempo: &tempo, sr: 48_000.0, seg_start: s };
            c.render_audio(start, limit, s, s + 100, &ctx, &mut buf);
            l.extend_from_slice(buf.channel(0));
            r.extend_from_slice(buf.channel(1));
            s += 100;
        }
        (l, r)
    }

    fn energy(x: &[f32]) -> f32 {
        x.iter().map(|v| v * v).sum()
    }

    #[test]
    fn a_pan_envelope_moves_the_energy_from_left_to_right() {
        let tone: Vec<f32> = (0..96_000).map(|i| 0.5 * (std::f32::consts::TAU * 220.0 * i as f32 / 48_000.0).sin()).collect();
        // 2 beats = 1 s at 120 bpm: hard left at the start, hard right at the end.
        let mut c = clip(0, 2 * PPQ, None);
        c.envelopes.push(lane(AutomationTarget::Pan, &[(0, -1.0), (2 * PPQ, 1.0)]));
        let compiled = audio_clip(&c, tone.clone(), None);
        let (l, r) = play_stereo(&compiled, 0, Some(2 * PPQ), 48_000);
        let q = 1_000..3_000;
        let end = 45_000..47_000;
        assert!(energy(&l[q.clone()]) > 10.0 * energy(&r[q.clone()]), "left at the start: {} vs {}", energy(&l[q.clone()]), energy(&r[q.clone()]));
        assert!(energy(&r[end.clone()]) > 10.0 * energy(&l[end.clone()]), "right at the end");
        // Halfway the sound is centred and at unity, as with no envelope at all.
        let plain = audio_clip(&clip(0, 2 * PPQ, None), tone, None);
        let (pl, _) = play_stereo(&plain, 0, Some(2 * PPQ), 48_000);
        let mid = 23_900..24_100;
        assert!((energy(&l[mid.clone()]) / energy(&pl[mid.clone()]) - 1.0).abs() < 0.05);
        assert!((energy(&r[mid.clone()]) / energy(&pl[mid]) - 1.0).abs() < 0.05);
    }

    #[test]
    fn a_pan_envelope_loops_with_the_clip_time_not_the_loop() {
        // The envelope is in clip time: a looping clip keeps sweeping while the loop repeats.
        let tone = vec![0.5f32; 24_000];
        let mut c = clip(0, 4 * PPQ, Some((0, PPQ)));
        c.envelopes.push(lane(AutomationTarget::Pan, &[(0, -1.0), (4 * PPQ, 1.0)]));
        let compiled = audio_clip(&c, tone, None);
        let (l, r) = play_stereo(&compiled, 0, Some(4 * PPQ), 96_000);
        assert!(energy(&l[2_000..4_000]) > 10.0 * energy(&r[2_000..4_000]));
        assert!(energy(&r[92_000..94_000]) > 10.0 * energy(&l[92_000..94_000]));
    }

    #[test]
    fn a_transpose_envelope_changes_the_speed_of_a_repitch_clip() {
        // A ramp: the value read tells where in the file playback is.
        let ramp: Vec<f32> = (0..96_000).map(|i| i as f32 / 96_000.).collect();
        // Constant +12 st: twice as fast.
        let mut c = clip(0, 8 * PPQ, None);
        c.envelopes.push(lane(AutomationTarget::Transpose, &[(0, 12.0)]));
        let up = play(&audio_clip(&c, ramp.clone(), None), 120., 0, Some(8 * PPQ), 1200);
        assert!((up[1000] - 2000. / 96_000.).abs() < 1e-3, "{}", up[1000]);
        // A sweep from 0 to +12 st over 4 beats: the position is the integral of the speed.
        let mut c = clip(0, 8 * PPQ, None);
        c.envelopes.push(lane(AutomationTarget::Transpose, &[(0, 0.0), (4 * PPQ, 12.0)]));
        let sweep = play(&audio_clip(&c, ramp, None), 120., 0, Some(8 * PPQ), 48_000);
        let model = c.clone();
        for n in [6_000usize, 20_000, 40_000] {
            // 2 beats per second: sample n is at tick n × 1920 / 48000.
            let tau = n as f64 * 1920.0 / 48_000.0;
            let steps = 20_000;
            let area: f64 = (0..steps).map(|i| 2f64.powf(model.transpose_at(((i as f64 + 0.5) * tau / steps as f64) as Tick) as f64 / 12.0) * tau / steps as f64).sum();
            let want = area * 48_000.0 / 960.0 / 2.0 / 96_000.0;
            assert!((sweep[n] as f64 - want).abs() < 2e-3, "sample {n}: {} vs {want}", sweep[n]);
        }
    }

    #[test]
    fn a_transpose_envelope_shifts_a_stretch_mode_without_moving_it_in_time() {
        use aitrack_model::Warp;
        let sine: Vec<f32> = (0..96_000).map(|i| 0.5 * (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin()).collect();
        let audio = Arc::new(DecodedAudio::from_planar(vec![sine], 48_000));
        let mut c = clip(0, 4 * PPQ, None);
        c.envelopes.push(lane(AutomationTarget::Transpose, &[(0, 12.0)]));
        for mode in [WarpMode::Complex, WarpMode::Tones, WarpMode::Beats] {
            let warp = Warp { enabled: true, mode, original_bpm: Some(120.0), ..Warp::default() };
            let compiled = CompiledClip::audio(ClipId(1), &c, AudioClipData::of_bpm(&warp, Some(audio.clone()), 120.0));
            let out = play(&compiled, 120.0, 0, Some(4 * PPQ), 96_000);
            // Still plays for the whole 2 s (a Re-Pitch clip would be done after 1 s).
            assert!(energy(&out[70_000..90_000]) > 1.0, "{mode:?} keeps the clip's duration");
            let seg = &out[8_000..32_000];
            let crossings = seg.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
            let hz = crossings as f32 * 48_000.0 / seg.len() as f32;
            assert!((hz - 880.0).abs() < 45.0, "{mode:?} +12 st expected ~880 Hz, got {hz}");
            assert!(out.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn a_transpose_envelope_that_is_flat_at_zero_changes_nothing() {
        let tone: Vec<f32> = (0..48_000).map(|i| 0.5 * (std::f32::consts::TAU * 330.0 * i as f32 / 48_000.0).sin()).collect();
        let plain = clip(0, 2 * PPQ, None);
        let mut flat = plain.clone();
        flat.envelopes.push(lane(AutomationTarget::Transpose, &[(0, 0.0), (PPQ, 0.0)]));
        let a = play(&audio_clip(&plain, tone.clone(), None), 120., 0, Some(2 * PPQ), 30_000);
        let b = play(&audio_clip(&flat, tone, None), 120., 0, Some(2 * PPQ), 30_000);
        for (n, (x, y)) in a.iter().zip(&b).enumerate() {
            assert!((x - y).abs() < 1e-3, "sample {n}: {x} vs {y}");
        }
    }

    #[test]
    fn the_pitch_area_table_is_the_integral_of_the_speed() {
        let mut c = clip(0, 8 * PPQ, None);
        c.envelopes.push(lane(AutomationTarget::Transpose, &[(0, -12.0), (2 * PPQ, 12.0)]));
        let shape = ClipShape::of(&c);
        for t in [0.0, 100.0, 960.0, 1920.0, 5000.0, 40_000.0] {
            let steps = 100_000;
            let want: f64 = (0..steps).map(|i| 2f64.powf(shape.semitones((i as f64 + 0.5) * t / steps as f64) / 12.0) * t / steps as f64).sum();
            assert!((shape.pitch_integral(t) - want).abs() < 1e-3 * want.max(1.0), "t {t}: {} vs {want}", shape.pitch_integral(t));
        }
        // No envelope: the identity.
        assert_eq!(ClipShape::of(&clip(0, PPQ, None)).pitch_integral(123.5), 123.5);
    }

    #[test]
    fn a_stretched_clip_plays_the_same_audio_over_its_new_length() {
        use aitrack_model::{ops, Warp};
        // A ramp of four seconds: the value read tells where in the file playback is.
        let ramp: Vec<f32> = (0..192_000).map(|i| i as f32 / 192_000.).collect();
        let audio = Arc::new(DecodedAudio::from_planar(vec![ramp], 48_000));
        // Beats 1..3 of the file at 120 BPM (frames 24 000..72 000), stretched to four beats.
        let one_shot = clip(PPQ, 2 * PPQ, None);
        let (long, warp) = ops::stretched(&one_shot, &Warp::default(), 120.0, 4 * PPQ).unwrap();
        let c = CompiledClip::audio(ClipId(1), &long, AudioClipData::of(&warp, Some(audio.clone())));
        let out = play(&c, 120., 0, Some(long.length), 100_000);
        // Half speed: sample n reads frame 24 000 + n / 2, up to the old end at the new one.
        for n in [1_000usize, 48_000, 95_000] {
            let want = (24_000. + n as f32 / 2.) / 192_000.;
            assert!((out[n] - want).abs() < 1e-4, "sample {n}: {} vs {want}", out[n]);
        }
        assert!(out[96_100..].iter().all(|s| *s == 0.), "silent after the four beats");
        // Squeezed to one beat: twice as fast, with a transpose on top (+12 st: four times).
        let (short, mut warp) = ops::stretched(&one_shot, &Warp::default(), 120.0, PPQ).unwrap();
        warp.transpose = 12.;
        let c = CompiledClip::audio(ClipId(1), &short, AudioClipData::of(&warp, Some(audio)));
        let out = play(&c, 120., 0, Some(short.length), 20_000);
        let want = (24_000. + 4. * 2_000.) / 192_000.;
        assert!((out[2_000] - want).abs() < 1e-4, "{} vs {want}", out[2_000]);
    }

    #[test]
    fn reverse_reads_the_file_backwards() {
        let ramp: Vec<f32> = (0..4_800).map(|i| i as f32).collect();
        let c = clip(0, 8 * PPQ, None);
        // Exact frames when the speed is unchanged.
        let back = play(&pitched_clip(&c, ramp.clone(), 1.0, true), 120., 0, Some(8 * PPQ), 1000);
        assert_eq!(back[100..103], [4_699., 4_698., 4_697.]);
        // And through the interpolator when transposed.
        let fast = play(&pitched_clip(&c, ramp, 2.0, true), 120., 0, Some(8 * PPQ), 1000);
        assert!((fast[500] - 3_799.).abs() < 0.5, "{}", fast[500]);
    }

    fn shaped_clip() -> Clip {
        let mut c = clip(0, 4 * PPQ, None);
        c.fade_in = Fade { length: 700, curve: 0.8 };
        c.fade_out = Fade { length: 900, curve: -0.4 };
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        lane.insert(AutomationPoint { tick: 200, value: 0.9, curve: 0.0 });
        lane.insert(AutomationPoint { tick: 1500, value: 0.2, curve: 0.7 });
        lane.insert(AutomationPoint { tick: 3000, value: 1.0, curve: -1.0 });
        c.envelopes.push(lane);
        c
    }

    #[test]
    fn shape_is_gain_at_on_every_tick() {
        let mut linear = clip(0, 4 * PPQ, None);
        linear.fade_in = Fade { length: 480, curve: 0.0 };
        linear.fade_out = Fade { length: 4 * PPQ - 480, curve: 0.0 };
        for c in [shaped_clip(), linear, clip(0, 4 * PPQ, None)] {
            let shape = ClipShape::of(&c);
            let mut seg = 0;
            for t in 0..=c.length {
                let got = shape.gain(t as f64, Some(c.length), &mut seg);
                assert!((got - c.gain_at(t)).abs() < 1e-5, "tick {t}: {got} vs {}", c.gain_at(t));
            }
            // The cursor may be anywhere: going back in time finds the segment again.
            for t in [3500, 100, 1499, 1500, 0, 2999, 3000] {
                assert!((shape.gain(t as f64, Some(c.length), &mut seg) - c.gain_at(t)).abs() < 1e-5, "tick {t}");
            }
        }
        // A clip that never ends has no fade-out; a disabled envelope is ignored.
        let mut c = shaped_clip();
        let shape = ClipShape::of(&c);
        assert!((shape.gain(3800.0, None, &mut 0) - 1.0).abs() < 1e-6);
        assert!(shape.gain(3800.0, Some(c.length), &mut 0) < 0.5);
        c.envelopes[0].enabled = false;
        assert!(ClipShape::of(&c).envelope.is_empty());
        assert!(!ClipShape::of(&clip(0, PPQ, None)).active());
    }

    #[test]
    fn loop_repeats_whole_frames_without_drift() {
        // 97 BPM: a tick is 30.93 samples, so neither the offset nor the loop
        // is a whole number of frames. The source holds its own frame index.
        let (bpm, offset, ls, le) = (97.0, 100, 40, 500);
        let data: Vec<f32> = (0..20_000).map(|i| i as f32).collect();
        let c = audio_clip(&clip(offset, 4 * PPQ, Some((ls, le))), data, None);
        let total = 3_000_000;
        let out = play(&c, bpm, 0, None, total);
        let spt = 48_000.0 * 60.0 / (bpm * PPQ as f64);
        let (off_e, ls_e, le_e) = (offset as f64 * spt, ls as f64 * spt, le as f64 * spt);
        let span = le_e - ls_e;
        let mut checked = 0;
        for (n, got) in out.iter().enumerate().skip(64) {
            let pos = off_e + n as f64;
            let exact = if pos < le_e { pos } else { ls_e + (pos - le_e).rem_euclid(span) };
            // Skip the crossfade before each wrap (5 ms) and the wrap itself.
            if le_e - exact < 245.0 || exact - ls_e < 3.0 {
                continue;
            }
            assert_eq!(got.fract(), 0.0, "sample {n} is not a whole frame: {got}");
            assert!((*got as f64 - exact).abs() <= 2.0, "sample {n}: frame {got}, exact {exact:.2}");
            checked += 1;
        }
        assert!(checked > total * 9 / 10);
        // More than 200 repeats were played.
        assert!(total as f64 / span > 200.0);
    }

    #[test]
    fn loop_seam_crossfades_into_the_lead_up() {
        // One beat of file; the loop is its second half.
        let ramp: Vec<f32> = (0..24_000).map(|i| i as f32 / 24_000.0).collect();
        let c = audio_clip(&clip(480, 8 * PPQ, Some((480, 960))), ramp.clone(), None);
        let out = play(&c, 120.0, 0, Some(8 * PPQ), 60_000);
        // Passes are 12 000 samples; the last 240 of each crossfade from the
        // tail (frames up to 23 999) into the lead-up (frames up to 11 999).
        for pass in 0..4usize {
            let end = (pass + 1) * 12_000;
            for left in 1..=240usize {
                let w = (240 - left + 1) as f32 / 241.0;
                let want = ramp[24_000 - left] * (1.0 - w) + ramp[12_000 - left] * w;
                assert!((out[end - left] - want).abs() < 1e-6, "pass {pass}, {left} before the wrap");
            }
            assert_eq!(out[end], ramp[12_000]);
            assert_eq!(out[end - 241], ramp[24_000 - 241]);
            // The half-file jump is spread over the crossfade.
            let step = (end - 241..end).map(|i| (out[i + 1] - out[i]).abs()).fold(0.0f32, f32::max);
            assert!(step < 0.5 / 241.0 * 1.1, "pass {pass}: step {step}");
        }
        // A loop from the start of the file has no lead-up: the tail fades
        // into the first frame, so the wrap is continuous all the same.
        let c = audio_clip(&clip(0, 8 * PPQ, Some((0, 960))), ramp.iter().map(|v| v + 0.25).collect(), None);
        let out = play(&c, 120.0, 0, Some(8 * PPQ), 60_000);
        assert_eq!(out[23_000], ramp[23_000] + 0.25);
        assert!((out[23_999] - 0.25).abs() < 0.01, "{}", out[23_999]);
        assert_eq!(out[24_000], 0.25);
        let step = (23_700..24_100).map(|i| (out[i + 1] - out[i]).abs()).fold(0.0f32, f32::max);
        assert!(step < 1.0 / 241.0 * 1.1, "step {step}");
    }

    #[test]
    fn warped_loop_crossfades_too() {
        // Material at 60 BPM in a 120 BPM project plays twice as fast: the
        // half-beat loop is 24 000 frames played in 12 000 samples, and the
        // 5 ms seam lasts 120 samples.
        let ramp: Vec<f32> = (0..48_000).map(|i| 0.25 + i as f32 / 96_000.0).collect();
        let c = audio_clip(&clip(480, 8 * PPQ, Some((480, 960))), ramp.clone(), Some(60.0));
        let out = play(&c, 120.0, 0, Some(8 * PPQ), 30_000);
        // Away from the seam: the source at double speed (frame 24 000 + 2n).
        for n in [100usize, 3000, 9000, 11_800, 12_100, 15_000, 23_800, 24_200] {
            let frame = 24_000 + 2 * (n % 12_000);
            assert!((out[n] - ramp[frame]).abs() < 1e-3, "sample {n}: {} vs {}", out[n], ramp[frame]);
        }
        for wrap in [12_000usize, 24_000] {
            let step = (wrap - 200..wrap + 20).map(|i| (out[i + 1] - out[i]).abs()).fold(0.0f32, f32::max);
            assert!(step < 0.25 / 120.0 * 1.2, "wrap at {wrap}: step {step}");
        }
        // Fades follow clip time on the warped path as well.
        let mut faded = clip(0, 2 * PPQ, None);
        faded.fade_in = Fade { length: 480, curve: 0.6 };
        faded.fade_out = Fade { length: 480, curve: 0.0 };
        let c = audio_clip(&faded, vec![1.0; 96_000], Some(60.0));
        let out = play(&c, 120.0, 0, Some(2 * PPQ), 50_000);
        for t in [0, 10, 200, 479, 480, 1000, 1441, 1700, 1919] {
            assert!((out[25 * t as usize] - faded.gain_at(t)).abs() < 1e-3, "tick {t}: {} vs {}", out[25 * t as usize], faded.gain_at(t));
        }
        assert!(out[48_000].abs() < 1e-6);
    }

    /// Two files of one length that agree at a handful of sample points (two
    /// stems silent in the same places) must not share a stretched render,
    /// and a slightly different stretch gets its own.
    #[test]
    fn warp_cache_tells_sources_and_stretches_apart() {
        let n = 24_000usize;
        let tone = |hz: f32| {
            let mut d: Vec<f32> = (0..n).map(|i| (i as f32 * hz * std::f32::consts::TAU / 48_000.0).sin() * 0.5).collect();
            for step in 0..8 {
                d[step * n / 8] = 0.0;
            }
            d[n - 1] = 0.0;
            Arc::new(DecodedAudio::from_planar(vec![d], 48_000))
        };
        let (a, b) = (tone(220.0), tone(1760.0));
        let cache = SampleCache::blocking();
        let cached_warp = |src: &Arc<DecodedAudio>, mode, scale, pitch, cents| cache.warped(src, mode, scale, pitch, cents).expect("blocking cache renders now");
        let (wa, _) = cached_warp(&a, WarpMode::Complex, 1.25, 1.0, 0);
        let (wb, _) = cached_warp(&b, WarpMode::Complex, 1.25, 1.0, 0);
        assert!(!Arc::ptr_eq(&wa, &wb), "another file's render was reused");
        let (again, _) = cached_warp(&a, WarpMode::Complex, 1.25, 1.0, 0);
        assert!(Arc::ptr_eq(&wa, &again), "the same file and stretch missed the cache");
        let (near, _) = cached_warp(&a, WarpMode::Complex, 1.2504, 1.0, 0);
        assert!(!Arc::ptr_eq(&wa, &near), "a different stretch was reused");
    }

    /// One second of a 440 Hz sine: a whole number of cycles, so copies laid
    /// end to end are one continuous tone.
    fn whole_cycle_sine() -> Vec<f32> {
        (0..48_000).map(|i| 0.5 * (std::f64::consts::TAU * 440.0 * i as f64 / 48_000.0).sin() as f32).collect()
    }

    /// Three clips that each play a whole file, back to back, are one tone:
    /// the end of a clip that cuts nothing is not ramped down.
    #[test]
    fn contiguous_clips_that_cut_nothing_join_without_a_notch() {
        let sine = whole_cycle_sine();
        // 120 BPM, 48 kHz: 1 s = 2 beats = 1920 ticks.
        let c = clip(0, 2 * PPQ, None);
        let compiled = audio_clip(&c, sine.clone(), None);
        let mut mix = vec![0.0f32; 144_000];
        for k in 0..3 {
            let out = play(&compiled, 120.0, k * 2 * PPQ, Some(2 * PPQ), 144_000);
            for (m, o) in mix.iter_mut().zip(&out) {
                *m += o;
            }
        }
        let worst = mix.iter().enumerate().map(|(n, v)| (v - sine[n % 48_000]).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "deviation from the continuous sine: {worst}");
    }

    /// A clip that ends before its file does still ramps down at its end, on
    /// both the whole-frame path and the interpolated one.
    #[test]
    fn a_clip_that_cuts_material_keeps_its_end_ramp() {
        let tone = vec![1.0f32; 192_000];
        let c = clip(0, 2 * PPQ, None);
        let out = play(&audio_clip(&c, tone.clone(), None), 120.0, 0, Some(2 * PPQ), 48_100);
        assert!((out[48_000 - 32] - 0.5).abs() < 1e-3 && (out[48_000 - 64] - 1.0).abs() < 1e-3, "{}", out[48_000 - 32]);
        assert!(out[47_999] < 0.02);
        // Twice as fast: the same second consumes two seconds of the file.
        let fast = play(&pitched_clip(&c, tone.clone(), 2.0, false), 120.0, 0, Some(2 * PPQ), 48_100);
        assert!((fast[48_000 - 32] - 0.5).abs() < 1e-3, "{}", fast[48_000 - 32]);
        // ...and when the file ends exactly where the clip does, there is no cut.
        let whole = play(&pitched_clip(&c, vec![1.0; 96_000], 2.0, false), 120.0, 0, Some(2 * PPQ), 48_100);
        assert!((whole[48_000 - 42] - 1.0).abs() < 1e-3, "{}", whole[48_000 - 42]);
        // A looping clip is cut wherever it ends.
        let looped = play(&audio_clip(&clip(0, 2 * PPQ, Some((0, PPQ))), vec![1.0; 24_000], None), 120.0, 0, Some(2 * PPQ), 48_100);
        assert!((looped[48_000 - 32] - 0.5).abs() < 0.05, "{}", looped[48_000 - 32]);
    }

    /// With a fractional start (a beat at 126 BPM is 22 857.14 samples) the
    /// first frame of the file used to fall one sample before the range the
    /// arrangement hands over, and was lost.
    #[test]
    fn frame_zero_is_played_at_a_fractional_start() {
        let file: Vec<f32> = (0..60_000).map(|i| (i + 1) as f32 / 60_000.0).collect();
        let c = audio_clip(&clip(0, 2 * PPQ, None), file.clone(), None);
        let tempo = TempoMap::constant(126.0);
        for half_beats in 0..8 {
            let start = half_beats * PPQ / 2;
            let out = play(&c, 126.0, start, Some(2 * PPQ), 100_000);
            let at = (tempo.tick_to_samples(start, 48_000.0) - 1e-6).ceil() as usize;
            let first = out.iter().position(|v| *v != 0.0).expect("the clip sounds");
            assert_eq!(first, at, "start tick {start}");
            assert_eq!(out[first], file[0], "start tick {start}: frame 0 is missing");
            assert_eq!(out[first + 1000], file[1000], "start tick {start}");
        }
    }

    /// A warp that happens to sit at the project's tempo plays the file as it
    /// is, not through the 16-tap interpolator.
    #[test]
    fn warp_at_the_project_tempo_is_bit_exact() {
        use aitrack_model::Warp;
        let mut seed = 0x2545_F491u32;
        let noise: Vec<f32> = (0..48_000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as f32 / u32::MAX as f32 - 0.5
            })
            .collect();
        let audio = Arc::new(DecodedAudio::from_planar(vec![noise.clone()], 48_000));
        let c = clip(0, 2 * PPQ, None);
        for mode in [WarpMode::Repitch, WarpMode::Beats, WarpMode::Complex] {
            let warp = Warp { enabled: true, mode, original_bpm: Some(120.0), ..Warp::default() };
            let compiled = CompiledClip::audio(ClipId(1), &c, AudioClipData::of_bpm(&warp, Some(audio.clone()), 120.0));
            let out = play(&compiled, 120.0, 0, Some(2 * PPQ), 48_000);
            let residual: f64 = out.iter().zip(&noise).take(47_900).map(|(a, b)| ((a - b) as f64).powi(2)).sum();
            let signal: f64 = noise.iter().take(47_900).map(|b| (*b as f64).powi(2)).sum();
            let db = 10.0 * (residual / signal).max(1e-30).log10();
            assert!(db < -120.0, "{mode:?}: residual {db:.1} dB");
        }
        // A real stretch is not taken for it, and neither is a clip whose
        // tempo moves while it plays.
        let warp = Warp { enabled: true, mode: WarpMode::Repitch, original_bpm: Some(60.0), ..Warp::default() };
        assert!(!AudioClipData::of_bpm(&warp, Some(audio.clone()), 120.0).unity);
        let warp = Warp { enabled: true, mode: WarpMode::Repitch, original_bpm: Some(120.0), transpose: 1.0, ..Warp::default() };
        assert!(!AudioClipData::of_bpm(&warp, Some(audio), 120.0).unity);
        let mut moving = TempoMap::constant(120.0);
        moving.set_point(aitrack_model::time::TempoPoint { start: 4 * PPQ, bpm: 140.0, ramp: false });
        assert!(tempo_is_steady(&moving, 0, 4 * PPQ, 120.0));
        assert!(!tempo_is_steady(&moving, 0, 5 * PPQ, 120.0));
        assert!(!tempo_is_steady(&moving, 5 * PPQ, 6 * PPQ, 120.0), "starts in a 140 BPM region");
        assert!(!tempo_is_steady(&moving, 0, Tick::MAX, 120.0));
        moving.set_point(aitrack_model::time::TempoPoint { start: 0, bpm: 120.0, ramp: true });
        assert!(!tempo_is_steady(&moving, 0, PPQ, 120.0), "a ramp is not steady");
    }

    /// A warp that is only close to the project's tempo must keep following
    /// the grid: reading it frame by frame would drift (120.01 against 120 is
    /// 1200 samples after five minutes).
    #[test]
    fn a_nearly_matching_warp_still_follows_the_grid() {
        use aitrack_model::Warp;
        let warp = |bpm: f64| Warp { enabled: true, mode: WarpMode::Repitch, original_bpm: Some(bpm), ..Warp::default() };
        let sawtooth: Vec<f32> = (0..14_450_000usize).map(|i| (i % 1000) as f32 / 1000.0).collect();
        let audio = Arc::new(DecodedAudio::from_planar(vec![sawtooth], 48_000));
        assert!(!AudioClipData::of_bpm(&warp(120.01), Some(audio.clone()), 120.0).unity);
        assert!(!AudioClipData::of_bpm(&warp(120.0 + 1e-6), Some(audio.clone()), 120.0).unity);
        assert!(AudioClipData::of_bpm(&warp(120.0), Some(audio.clone()), 120.0).unity);
        // Five minutes in: 600 beats.
        let length = 600 * PPQ;
        let c = CompiledClip::audio(ClipId(1), &clip(0, length, None), AudioClipData::of_bpm(&warp(120.01), Some(audio), 120.0));
        let tempo = TempoMap::constant(120.0);
        let at_frame = |n: i64| n as f64 * 120.0 / 120.01;
        // A sample where the source is mid-tooth, so the value tells the frame.
        let n = (14_300_000i64..).find(|n| (400.0..600.0).contains(&(at_frame(*n) % 1000.0))).unwrap();
        let mut buf = AudioBuffer::new(2, 1);
        let ctx = TimeCtx { tempo: &tempo, sr: 48_000.0, seg_start: n };
        c.render_audio(0, Some(length), n, n + 1, &ctx, &mut buf);
        let got = buf.channel(0)[0] as f64 * 1000.0;
        let want = at_frame(n) % 1000.0;
        assert!((got - want).abs() < 1.0, "sample {n}: source frame {got:.2} (mod 1000), the grid says {want:.2}");
    }

    /// A clip that ends a few samples before its file does cuts material,
    /// however small the rest: only an end at the end of the file skips the ramp.
    #[test]
    fn the_end_ramp_goes_by_samples_not_ticks() {
        let ends_short = play(&audio_clip(&clip(0, 2 * PPQ, None), vec![1.0; 48_020], None), 120.0, 0, Some(2 * PPQ), 48_100);
        assert!((ends_short[48_000 - 32] - 0.5).abs() < 1e-3, "20 samples short: {}", ends_short[48_000 - 32]);
        let ends_at = play(&audio_clip(&clip(0, 2 * PPQ, None), vec![1.0; 48_000], None), 120.0, 0, Some(2 * PPQ), 48_100);
        assert!((ends_at[48_000 - 32] - 1.0).abs() < 1e-6, "at the end: {}", ends_at[48_000 - 32]);
    }

    /// A project with an audio track that holds `n` warped clips, each over a
    /// file of its own (2 s of a tone) recorded at 100 BPM, in a 120 BPM project.
    fn warped_project(n: usize, samples: &SampleCache) -> aitrack_model::Project {
        use aitrack_model::{AudioSource, Color, Placement, Project, Source, Track, TrackKind, Warp};
        let mut p = Project::new("warps");
        p.tempo = TempoMap::constant(120.0);
        let tid = p.alloc_track_id();
        p.tracks.push(Track::new(tid, TrackKind::Audio, "audio", Color(0)));
        for i in 0..n {
            let path = std::path::PathBuf::from(format!("mem://warp{i}.wav"));
            let tone: Vec<f32> = (0..96_000).map(|k| 0.4 * (k as f32 * (200.0 + i as f32) * std::f32::consts::TAU / 48_000.0).sin()).collect();
            samples.insert(path.clone(), Arc::new(DecodedAudio::from_planar(vec![tone], 48_000)));
            let sid = p.alloc_source_id();
            let warp = Warp { enabled: true, mode: WarpMode::Complex, original_bpm: Some(100.0), ..Warp::default() };
            p.sources.insert(sid, Source::Audio(AudioSource { path, hash: None, sample_rate: 48_000, channels: 1, frames: 96_000, warp, key: None }));
            let cid = p.alloc_clip_id();
            p.clips.insert(cid, clip_for(sid));
            let pid = p.alloc_placement_id();
            p.tracks.last_mut().unwrap().arrangement.push(Placement { id: pid, clip: cid, start: i as Tick * 4 * PPQ, lane: 0 });
        }
        p
    }

    fn clip_for(source: aitrack_model::SourceId) -> Clip {
        Clip::new("c", aitrack_model::Color(0), source, 4 * PPQ)
    }

    fn compile_timed(p: &aitrack_model::Project, samples: &SampleCache) -> (std::time::Duration, crate::CompiledGraph) {
        let mut pool = crate::shared::ProcessorPool::new();
        let mut cx = crate::compile::CompileContext {
            factory: &crate::fallback::FallbackFactory,
            pool: &mut pool,
            samples,
            sample_rate: 48_000.0,
            max_frames: 256,
            prev_meters: None,
            taps: Vec::new(),
            analysis: None,
        };
        let t = std::time::Instant::now();
        let graph = crate::compile::compile(p, &mut cx).expect("compiles");
        (t.elapsed(), graph)
    }

    /// Wait (up to 30 s) until the loader has nothing left to render.
    fn wait_idle(samples: &SampleCache) {
        let t = std::time::Instant::now();
        while samples.is_loading() && t.elapsed() < std::time::Duration::from_secs(30) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!samples.is_loading(), "the warp worker did not finish");
    }

    /// Stretching a file no longer happens while the graph compiles: the
    /// clip plays unstretched, the worker renders, and a recompile picks the
    /// render up. Compiling 40 warped clips took 289 ms (the 32-entry cache
    /// thrashed, so even a warm compile redid the lot).
    #[test]
    fn warps_render_off_the_control_thread() {
        use aitrack_model::Warp;
        let notified = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = notified.clone();
        let samples = SampleCache::with_loader(move || {
            n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        let p = warped_project(40, &samples);
        // Cold: nothing is rendered yet and the compile does not wait for it.
        // (The first compile is the one that queues the renders.)
        let cold = compile_timed(&p, &samples).0;
        assert!(cold < std::time::Duration::from_millis(5), "compile with 40 warped clips took {cold:?}");
        assert_eq!(samples.warp_bytes(), 0, "nothing finished yet or it was counted twice");
        wait_idle(&samples);
        assert!(notified.load(std::sync::atomic::Ordering::SeqCst) >= 1, "no recompile was asked for");
        // The renders count against the budget and show in the memory use.
        let warp_bytes = samples.warp_bytes();
        assert!(warp_bytes > 40 * 96_000, "{warp_bytes} bytes of renders");
        assert!(samples.used_bytes() >= warp_bytes + 40 * 96_000 * 4);
        // Warm: every render is there, the compile stays fast.
        let warm = compile_timed(&p, &samples).0;
        assert!(warm < std::time::Duration::from_millis(5), "warm compile took {warm:?}");
        println!("compile, 40 warped clips: cold {cold:?}, warm {warm:?}");

        // One clip, step by step: Re-Pitch while rendering, stretched after.
        let tone: Vec<f32> = (0..96_000).map(|k| 0.4 * (k as f32 * 300.0 * std::f32::consts::TAU / 48_000.0).sin()).collect();
        let src = Arc::new(DecodedAudio::from_planar(vec![tone], 48_000));
        let warp = Warp { enabled: true, mode: WarpMode::Tones, original_bpm: Some(100.0), ..Warp::default() };
        let first = AudioClipData::of_bpm_in(&warp, Some(src.clone()), 120.0, &samples);
        assert!(first.warped && !first.pitch_preserving, "it should play in Re-Pitch meanwhile");
        assert!(Arc::ptr_eq(first.audio.as_ref().unwrap(), &src));
        wait_idle(&samples);
        let then = AudioClipData::of_bpm_in(&warp, Some(src.clone()), 120.0, &samples);
        assert!(then.warped && then.pitch_preserving);
        // 100 -> 120 BPM: shorter by 100/120.
        let frames = then.audio.as_ref().unwrap().frames as f64;
        assert!((frames / 96_000.0 - 100.0 / 120.0).abs() < 0.01, "{frames} frames");
        // The same render is found again, by identity and exact factor.
        let again = AudioClipData::of_bpm_in(&warp, Some(src), 120.0, &samples);
        assert!(Arc::ptr_eq(then.audio.as_ref().unwrap(), again.audio.as_ref().unwrap()));
    }

    /// While a stretch renders (and for good if it fails) the clip plays in
    /// Re-Pitch: on the grid, pitch shifted by the tempo ratio. The render
    /// then takes over at the same position, and a failure is recorded.
    #[test]
    fn a_clip_waiting_for_its_warp_stays_on_the_grid() {
        use aitrack_model::Warp;
        // The file is a ramp: the value read says which frame plays.
        let ramp: Vec<f32> = (0..96_000).map(|i| i as f32 / 96_000.0).collect();
        let src = Arc::new(DecodedAudio::from_planar(vec![ramp], 48_000));
        // 100 BPM material in a 120 BPM project.
        let warp = Warp { enabled: true, mode: WarpMode::Tones, original_bpm: Some(100.0), ..Warp::default() };
        let c = clip(0, 4 * PPQ, None);
        let samples = SampleCache::with_loader(|| {});
        let pending = AudioClipData::of_bpm_in(&warp, Some(src.clone()), 120.0, &samples);
        assert!(pending.warped && !pending.pitch_preserving, "Re-Pitch meanwhile");
        // On the grid: sample n reads frame n * 120 / 100 (6000 samples = tick 120).
        let out = play(&CompiledClip::audio(ClipId(1), &c, pending), 120.0, 0, Some(4 * PPQ), 30_000);
        for n in [1_000usize, 5_000, 20_000, 29_000] {
            let frame = (out[n] * 96_000.0) as f64;
            assert!((frame - n as f64 * 1.2).abs() <= 1.0, "sample {n}: frame {frame}");
        }
        // The render arrives: the same tick maps to the same place in the file.
        wait_idle(&samples);
        let ready = AudioClipData::of_bpm_in(&warp, Some(src.clone()), 120.0, &samples);
        assert!(ready.pitch_preserving);
        let (before, after) = (AudioClipData::of_bpm_in(&warp, Some(src.clone()), 120.0, &SampleCache::with_loader(|| {})), ready);
        let spt = 25.0;
        for tick in [0.0, 100.0, 777.0, 1500.0, 3000.0, 3839.0] {
            let was = before.source_frame(tick, spt) / before.audio.as_ref().unwrap().frames as f64;
            let now = after.source_frame(tick, spt) / after.audio.as_ref().unwrap().frames as f64;
            let drift = (was - now) * after.audio.as_ref().unwrap().frames as f64;
            assert!(drift.abs() <= 1.0, "tick {tick}: {drift:.2} samples apart after the switch");
        }
    }

    #[test]
    fn a_failed_warp_stays_in_repitch_and_is_recorded() {
        use aitrack_model::Warp;
        let src = Arc::new(DecodedAudio::from_planar(vec![vec![0.5; 48_000]], 48_000));
        let warp = Warp { enabled: true, mode: WarpMode::Complex, original_bpm: Some(100.0), ..Warp::default() };
        let samples = SampleCache::with_loader_and_warper(|| {}, |_, _, _, _| panic!("boom"));
        let first = AudioClipData::of_bpm_in(&warp, Some(src.clone()), 120.0, &samples);
        assert!(first.warped && !first.pitch_preserving);
        wait_idle(&samples);
        let after = AudioClipData::of_bpm_in(&warp, Some(src.clone()), 120.0, &samples);
        assert!(after.warped && !after.pitch_preserving, "a failed render leaves the clip in Re-Pitch");
        assert!((after.source_frame(960.0, 25.0) - 960.0 * 25.0 * 1.2).abs() < 1.0, "still on the grid");
        let failures = samples.warp_failures();
        assert_eq!(failures.len(), 1, "{failures:?}");
        // The blocking cache (offline renders) behaves the same.
        let blocking = SampleCache::with_warper(|_, _, _, _| panic!("boom"));
        let data = AudioClipData::of_bpm_in(&warp, Some(src), 120.0, &blocking);
        assert!(data.warped && !data.pitch_preserving);
        assert_eq!(blocking.warp_failures().len(), 1);
    }

    /// Renders are evicted with the files when the budget is short, and a
    /// render of a file that is gone goes with it.
    #[test]
    fn warp_renders_obey_the_budget() {
        let samples = SampleCache::blocking();
        let make = |hz: f32| Arc::new(DecodedAudio::from_planar(vec![(0..48_000).map(|i| (i as f32 * hz * 0.01).sin()).collect()], 48_000));
        let (a, b) = (make(1.0), make(2.0));
        let (wa, _) = samples.warped(&a, WarpMode::Complex, 1.25, 1.0, 0).unwrap();
        let (_wb, _) = samples.warped(&b, WarpMode::Complex, 1.25, 1.0, 0).unwrap();
        let both = samples.warp_bytes();
        assert!(both >= 2 * wa.byte_size());
        // Room for one render: the older one that nobody holds is dropped.
        drop(wa);
        samples.set_budget_bytes(both - 1);
        assert!(samples.warp_bytes() < both);
        assert!(samples.used_bytes() < both || samples.warp_bytes() == 0);
        // A render whose source is gone is forgotten.
        drop(a);
        drop(b);
        samples.set_budget_bytes(usize::MAX / 2);
        samples.insert("mem://x.wav", make(3.0));
        assert_eq!(samples.warp_bytes(), 0, "{} bytes of renders of vanished files", samples.warp_bytes());
    }

    #[test]
    fn odd_loops_and_files_are_safe() {
        let ones = || vec![1.0f32; 6000];
        // (The interpolator may overshoot a little at the edges of the file.)
        let finite = |out: &[f32]| out.iter().all(|s| s.is_finite() && s.abs() <= 1.2);
        for warp in [None, Some(120.0)] {
            // Offset past the loop end: starts inside the loop, where
            // playback would have wrapped to ((705 − 10) mod 30 = 5 ticks in).
            let data: Vec<f32> = (0..6000).map(|i| i as f32 / 6000.0).collect();
            let c = audio_clip(&clip(705, 4 * PPQ, Some((10, 40))), data.clone(), warp);
            let out = play(&c, 120.0, 0, Some(4 * PPQ), 4000);
            assert!(finite(&out));
            assert!((out[100] - data[375 + 100]).abs() < 1e-3, "{warp:?}: {}", out[100]);
            // A loop region longer than the file: silence past the end.
            let c = audio_clip(&clip(0, 8 * PPQ, Some((0, 960))), ones(), warp);
            let out = play(&c, 120.0, 0, Some(8 * PPQ), 60_000);
            assert!(finite(&out));
            assert!((out[3000] - 1.0).abs() < 1e-3 && (out[24_000 + 3000] - 1.0).abs() < 1e-3);
            assert!(out[6100..23_000].iter().all(|s| s.abs() < 1e-3), "{warp:?}: sound past the end of the file");
            // The offset itself past the end of the file, with and without a loop.
            for looping in [None, Some((0, 100_000))] {
                let c = audio_clip(&clip(50_000, 4 * PPQ, looping), ones(), warp);
                assert!(play(&c, 120.0, 0, Some(4 * PPQ), 4000).iter().all(|s| *s == 0.0));
            }
            // An empty file, a one-tick loop, a negative loop start, a loop
            // that never ends (Session View).
            let c = audio_clip(&clip(0, 4 * PPQ, Some((0, 480))), Vec::new(), warp);
            assert!(play(&c, 120.0, 0, None, 4000).iter().all(|s| *s == 0.0));
            let c = audio_clip(&clip(0, 4 * PPQ, Some((100, 101))), ones(), warp);
            assert!(finite(&play(&c, 120.0, 0, None, 4000)));
            let c = audio_clip(&clip(-50, 4 * PPQ, Some((-200, 100))), ones(), warp);
            assert!(finite(&play(&c, 120.0, 0, None, 20_000)));
        }
        // An inverted or empty loop region is no loop at all.
        let c = audio_clip(&clip(0, 4 * PPQ, Some((480, 480))), ones(), None);
        assert!(c.looping.is_none());
        let out = play(&c, 120.0, 0, Some(4 * PPQ), 8000);
        assert!(out[..6000].iter().all(|s| *s == 1.0) && out[6000..].iter().all(|s| *s == 0.0));
    }
}
