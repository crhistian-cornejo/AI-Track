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
//! warp they play at their native speed. With `warp.enabled` and an
//! `original_bpm` they are *repitched* (speed follows the tempo). Pitch-
//! preserving stretch plugs in at [`AudioClipData::source_frame`].

use crate::sample_cache::DecodedAudio;
use aitrack_dsp::event::Expression;
use aitrack_dsp::{AudioBuffer, Event, EventKind};
use aitrack_model::{Clip, ClipId, FollowAction, LaunchMode, NoteSource, Quantize, TempoMap, Tick, PPQ};
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
        Self::base(id, clip, looping, ClipContent::Audio(data))
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
    /// (whose sample 0 is `ctx.seg_start`).
    #[allow(clippy::too_many_arguments)]
    pub fn render_audio(&self, start: Tick, limit: Option<Tick>, lo: i64, hi: i64, ctx: &TimeCtx<'_>, buf: &mut AudioBuffer) {
        let ClipContent::Audio(data) = &self.content else { return };
        let Some(audio) = &data.audio else { return };
        if self.muted || hi <= lo || buf.channels() < 2 {
            return;
        }
        const FADE: f64 = 64.0;
        let start_exact = ctx.tempo.tick_to_samples(start, ctx.sr);
        let spt = ctx.sr * 60.0 / (ctx.tempo.bpm_at(start) * PPQ as f64);
        let end_sample = limit.map(|l| ctx.tempo.tick_to_samples(start + l, ctx.sr));
        let stereo = audio.channels.len() >= 2;
        let ratio = 2f64.powf(data.transpose as f64 / 12.0);
        // Clip time (ticks) at a sample: exact for unwarped, tempo map for warped.
        let clip_time = |n: i64| -> f64 {
            if data.warped { ctx.tick_of(n) - start as f64 } else { (n as f64 - start_exact) / spt }
        };
        let (l, r) = buf.stereo_mut();
        let mut n = lo;
        while n < hi {
            let chunk_end = (n + crate::automation::CONTROL_BLOCK as i64).min(hi);
            let t0 = clip_time(n);
            let t1 = clip_time(chunk_end);
            let len = (chunk_end - n) as f64;
            for k in n..chunk_end {
                let tau = t0 + (t1 - t0) * (k - n) as f64 / len;
                if tau < 0.0 || limit.is_some_and(|lim| tau >= lim as f64) {
                    continue;
                }
                let mut src = self.offset as f64 + tau * if data.warped { ratio } else { 1.0 };
                if let Some((ls, le)) = self.looping
                    && src >= le as f64 {
                        let span = (le - ls) as f64;
                        src = ls as f64 + (src - le as f64).rem_euclid(span);
                    }
                let frame = data.source_frame(src, spt);
                let mut g = self.gain;
                let since = k as f64 - start_exact;
                if since < FADE {
                    g *= (since / FADE).max(0.0) as f32;
                }
                if let Some(e) = end_sample {
                    let until = e - k as f64;
                    if until < FADE {
                        g *= (until / FADE).max(0.0) as f32;
                    }
                }
                let idx = (k - ctx.seg_start) as usize;
                let sl = audio.sample(0, frame);
                let sr = if stereo { audio.sample(1, frame) } else { sl };
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
    use aitrack_model::{Color, LaunchSettings, LoopRegion, Note, NoteId, SourceId};

    fn clip(offset: Tick, length: Tick, looping: Option<(Tick, Tick)>) -> Clip {
        Clip {
            name: "c".into(),
            color: Color(0),
            source: SourceId(1),
            offset,
            length,
            looping: looping.map(|(start, end)| LoopRegion { start, end }),
            gain_db: 0.0,
            envelopes: Vec::new(),
            launch: LaunchSettings::default(),
            muted: false,
            scale: None,
        }
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
}
