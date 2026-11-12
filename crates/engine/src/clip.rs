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
//! warp they play at their native speed, reading whole frames (looping or
//! not). With `warp.enabled` and an `original_bpm` they are *repitched*
//! (speed follows the tempo). Pitch-preserving stretch plugs in at
//! [`AudioClipData::source_frame`].
//!
//! The gain of an audio clip is its `gain_db` times its fades and volume
//! envelope ([`ClipShape`], the same curve as `Clip::gain_at`), evaluated per
//! sample on the clip's timeline. A looping clip crossfades for a few
//! milliseconds into the material that leads up to the loop start every time
//! it wraps (into the loop's first frame when the loop starts at the
//! beginning of the file), so a sample that does not loop perfectly does not
//! click.

use crate::sample_cache::DecodedAudio;
use aitrack_dsp::event::Expression;
use aitrack_dsp::{AudioBuffer, Event, EventKind};
use aitrack_model::{curve_shape, AutomationPoint, Clip, ClipId, Fade, FollowAction, LaunchMode, NoteSource, Quantize, TempoMap, Tick, PPQ};
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
    pub off_tick: Tick,
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

    /// Emit note-offs whose time falls before `hi` (stale ones at offset 0).
    pub fn release_due(&mut self, ctx: &TimeCtx<'_>, hi: i64, out: &mut EventList) {
        let mut i = 0;
        while i < self.notes.len() {
            let n = self.notes[i];
            let at = ctx.sample_of(n.off_tick).max(n.min_off);
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

    fn push(&mut self, note: ActiveNote, out: &mut EventList, offset: u32) {
        // A new note on a key that is still held (overlap): end the old one.
        if let Some(i) = self.notes.iter().position(|n| n.key == note.key) {
            let old = self.notes.swap_remove(i);
            out.push(offset, EventKind::NoteOff { channel: 0, key: old.key, velocity: old.release, note_id: old.note_id });
        }
        if self.notes.len() < self.notes.capacity() {
            self.notes.push(note);
        }
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
}

#[derive(Debug, Default)]
pub(crate) struct Section {
    pub len: Tick,
    pub notes: Vec<SNote>,
}

impl Section {
    fn from_notes(src: &NoteSource, from: Tick, to: Tick) -> Self {
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
            })
            .collect();
        notes.sort_by_key(|n| (n.start, n.key));
        Self { len: (to - from).max(0), notes }
    }
}

pub(crate) struct AudioClipData {
    pub audio: Option<Arc<DecodedAudio>>,
    /// Repitch warp: source plays at `original_bpm`.
    pub warped: bool,
    pub original_bpm: f64,
    /// Semitones (repitch only; pitch-preserving modes are a TODO).
    pub transpose: f32,
}

impl AudioClipData {
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

pub(crate) enum ClipContent {
    Midi { intro: Section, cycle: Option<Section> },
    Audio(AudioClipData),
}

/// Fades and volume envelope of an audio clip, copied out of the model when
/// the graph is compiled so the audio thread only reads them.
///
/// [`gain`](Self::gain) is `Clip::gain_at` on a fractional clip time: equal
/// to it on whole ticks, continuous in between.
#[derive(Debug, Default)]
pub(crate) struct ClipShape {
    pub fade_in: Fade,
    pub fade_out: Fade,
    /// Points of the enabled volume envelope, sorted by tick (clip time).
    pub envelope: Box<[AutomationPoint]>,
}

impl ClipShape {
    fn of(clip: &Clip) -> Self {
        // A curvature outside -1..=1 (or NaN) would put NaN in the audio.
        let curve = |c: f32| if c.is_finite() { c.clamp(-1.0, 1.0) } else { 0.0 };
        let fade = |f: Fade| Fade { length: f.length.max(0), curve: curve(f.curve) };
        let mut envelope: Vec<AutomationPoint> = clip
            .volume_envelope()
            .map(|lane| {
                lane.points
                    .iter()
                    .map(|p| AutomationPoint { tick: p.tick, value: if p.value.is_finite() { p.value } else { 1.0 }, curve: curve(p.curve) })
                    .collect()
            })
            .unwrap_or_default();
        envelope.sort_by_key(|p| p.tick);
        Self { fade_in: fade(clip.fade_in), fade_out: fade(clip.fade_out), envelope: envelope.into_boxed_slice() }
    }

    /// Whether the clip has any fade or envelope at all.
    #[inline]
    pub fn active(&self) -> bool {
        self.fade_in.length > 0 || self.fade_out.length > 0 || !self.envelope.is_empty()
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
        if let Some(v) = self.envelope_at(t, seg) {
            gain *= v.clamp(0.0, 1.0);
        }
        gain
    }

    #[inline]
    fn envelope_at(&self, t: f64, seg: &mut usize) -> Option<f32> {
        let pts = &self.envelope;
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
}

impl CompiledClip {
    pub fn midi(id: ClipId, clip: &Clip, notes: &NoteSource) -> Self {
        let looping = clip.looping.filter(|l| l.end > l.start).map(|l| (l.start, l.end));
        let (intro, cycle) = match looping {
            Some((ls, le)) => {
                let intro = if clip.offset < le { Section::from_notes(notes, clip.offset, le) } else { Section::default() };
                (intro, Some(Section::from_notes(notes, ls, le)))
            }
            None => (Section::from_notes(notes, clip.offset, clip.offset + clip.length.max(0)), None),
        };
        Self::base(id, clip, looping, ClipContent::Midi { intro, cycle })
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
        let mut emit = |sec_start: Tick, sec: &Section| {
            let from = wa - sec_start;
            let to = wb - sec_start;
            let i0 = sec.notes.partition_point(|n| n.start < from);
            for n in &sec.notes[i0..] {
                if n.start >= to {
                    break;
                }
                let on = sec_start + n.start;
                if on < 0 || limit.is_some_and(|l| on >= l) {
                    continue;
                }
                let at = ctx.sample_of(start + on);
                if at < lo || at >= hi {
                    continue;
                }
                if n.probability < 1.0 && rng.next_f32() >= n.probability {
                    continue;
                }
                let offset = (at - ctx.seg_start) as u32;
                let mut off = sec_start + n.end;
                if let Some(l) = limit {
                    off = off.min(l);
                }
                active.push(
                    ActiveNote { key: n.key, note_id: n.note_id, off_tick: start + off, min_off: at + 1, release: n.release },
                    out,
                    offset,
                );
                out.push(offset, EventKind::NoteOn { channel: 0, key: n.key, velocity: n.velocity, note_id: n.note_id });
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

    /// Mix the audio of this clip instance for samples `[lo, hi)` into `buf`
    /// (whose sample 0 is `ctx.seg_start`). The instance starts at `start`
    /// (timeline tick) and ends at clip time `limit`; `None` plays on forever
    /// (a looping clip in the Session View).
    ///
    /// Gain is the clip gain × fades × volume envelope at the clip time of
    /// each sample (timeline ticks since `start`). An edge without a fade
    /// keeps its 64-sample anti-click ramp: always at the end, and at the
    /// start when the clip is trimmed (a clip that starts at the very
    /// beginning of its file keeps the transient the file starts with).
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
        let ramp_in = self.offset != 0 && shape.fade_in.length == 0;
        let ramp_out = shape.fade_out.length == 0;
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
        // A clip that starts past its loop end starts inside the loop, at the
        // position playback would have wrapped to.
        let looping = self.looping.filter(|(ls, le)| le > ls);
        let offset = match looping {
            Some((ls, le)) if self.offset >= le => ls + self.offset.saturating_sub(ls).rem_euclid(le - ls),
            _ => self.offset,
        };
        let mut seg = 0usize;
        let (l, r) = buf.stereo_mut();

        // A clip at its native speed reads whole frames: playback is
        // bit-identical to the file instead of an interpolated copy of it.
        if !data.warped {
            let start_i = start_exact.round() as i64;
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
                        let mut sl = audio.frame(0, frame);
                        let mut sr = if stereo { audio.frame(1, frame) } else { sl };
                        if pass.seam > 0 {
                            let left = pass_end - j;
                            if left <= pass.seam {
                                // Fade the tail into what leads up to the loop start.
                                let w = (pass.seam - left + 1) as f32 / (pass.seam + 1) as f32;
                                let lead = (loop_start - left).max(lead_floor);
                                let pl = audio.frame(0, lead);
                                let pr = if stereo { audio.frame(1, lead) } else { pl };
                                sl += (pl - sl) * w;
                                sr += (pr - sr) * w;
                            }
                        }
                        let mut g = edge_gain(j, (j - start_i) as f64);
                        if shaped {
                            g *= shape.gain(t0 + dt * (j - n) as f64, limit, &mut seg);
                        }
                        let idx = (j - ctx.seg_start) as usize;
                        l[idx] += sl * g;
                        r[idx] += sr * g;
                    }
                    t0 = t1;
                    n = chunk_end;
                }
                k = run_end;
            }
            return;
        }

        // Repitch warp: the source position follows the tempo map and is read
        // through the interpolator.
        let ratio = 2f64.powf(data.transpose as f64 / 12.0);
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
        let mut n = lo;
        while n < hi {
            let chunk_end = (n + crate::automation::CONTROL_BLOCK as i64).min(hi);
            let t0 = ctx.tick_of(n) - start as f64;
            let t1 = ctx.tick_of(chunk_end) - start as f64;
            let len = (chunk_end - n) as f64;
            for k in n..chunk_end {
                let tau = t0 + (t1 - t0) * (k - n) as f64 / len;
                if tau < 0.0 || limit.is_some_and(|lim| tau >= lim as f64) {
                    continue;
                }
                let mut src = offset as f64 + tau * ratio;
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
                let mut sl = audio.sample(0, frame);
                let mut sr = if stereo { audio.sample(1, frame) } else { sl };
                if w > 0.0 {
                    let lead = data.source_frame(src - span, spt).max(lead_floor);
                    let pl = audio.sample(0, lead);
                    let pr = if stereo { audio.sample(1, lead) } else { pl };
                    sl += (pl - sl) * w;
                    sr += (pr - sr) * w;
                }
                let mut g = edge_gain(k, k as f64 - start_exact);
                if shaped {
                    g *= shape.gain(tau, limit, &mut seg);
                }
                let idx = (k - ctx.seg_start) as usize;
                l[idx] += sl * g;
                r[idx] += sr * g;
            }
            n = chunk_end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    // ───────────────────────────── Audio ─────────────────────────────

    use aitrack_model::{AutomationLane, AutomationTarget};

    fn audio_clip(clip: &Clip, data: Vec<f32>, original_bpm: Option<f64>) -> CompiledClip {
        let audio = Arc::new(DecodedAudio::from_planar(vec![data], 48_000));
        let data = AudioClipData { audio: Some(audio), warped: original_bpm.is_some(), original_bpm: original_bpm.unwrap_or(120.0), transpose: 0.0 };
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
