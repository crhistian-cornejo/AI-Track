//! The Step Sequencer clip (Live 13): a notes clip whose content is a grid of
//! steps instead of free notes.
//!
//! The pattern lives on the clip's [`NoteSource`] next to the notes it
//! produces. The notes are the *rendered* pattern (see [`render`]): the
//! commands that change the pattern re-render them in the same step, so the
//! engine, the arrangement, export, MIDI drag-out and every other consumer
//! of notes play a step clip without knowing it is one, and the pattern is
//! the only thing that has to be edited. While a source has a pattern, the
//! note commands (`AddNotes`, `RemoveNotes`, `UpdateNotes`) refuse it:
//! [`Command::SetStepPattern`] with no pattern converts it to plain notes.
//!
//! Each row is one key with its own length (steps), rate, direction and
//! swing, so rows of different lengths drift against each other
//! (polyrhythm) and all repeat over the pattern's span.

use crate::command::Command;
use crate::ids::*;
use crate::project::*;
use crate::time::{Tick, PPQ};
use serde::{Deserialize, Serialize};

/// Most steps a row can have.
pub const MAX_ROW_STEPS: u16 = 128;
/// Most hits a step can repeat (ratchet).
pub const MAX_REPEAT: u8 = 8;
/// Most rows a pattern can have.
pub const MAX_ROWS: usize = 32;
/// Steps of a new row.
pub const DEFAULT_ROW_STEPS: u16 = 16;
/// Fraction of its slot that a hit sounds.
const GATE: f64 = 0.8;

/// One step of a row.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub on: bool,
    /// 0..=1.
    #[serde(default = "default_velocity")]
    pub velocity: f32,
    /// Chance to play, 0..=1; becomes the note's `probability`.
    #[serde(default = "one")]
    pub chance: f32,
    /// Hits inside the step, 1..=[`MAX_REPEAT`] (a ratchet).
    #[serde(default = "one_hit")]
    pub repeat: u8,
}

fn default_velocity() -> f32 {
    0.8
}
fn one() -> f32 {
    1.0
}
fn one_hit() -> u8 {
    1
}

impl Default for Step {
    fn default() -> Self {
        Self { on: false, velocity: 0.8, chance: 1.0, repeat: 1 }
    }
}

/// How long a step lasts, as a note value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRate {
    Quarter,
    Eighth,
    EighthTriplet,
    #[default]
    Sixteenth,
    SixteenthTriplet,
    ThirtySecond,
}

impl StepRate {
    pub const ALL: [StepRate; 6] = [Self::Quarter, Self::Eighth, Self::EighthTriplet, Self::Sixteenth, Self::SixteenthTriplet, Self::ThirtySecond];

    /// Length of one step.
    pub fn ticks(self) -> Tick {
        match self {
            Self::Quarter => PPQ,
            Self::Eighth => PPQ / 2,
            Self::EighthTriplet => PPQ / 3,
            Self::Sixteenth => PPQ / 4,
            Self::SixteenthTriplet => PPQ / 6,
            Self::ThirtySecond => PPQ / 8,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Quarter => "1/4",
            Self::Eighth => "1/8",
            Self::EighthTriplet => "1/8T",
            Self::Sixteenth => "1/16",
            Self::SixteenthTriplet => "1/16T",
            Self::ThirtySecond => "1/32",
        }
    }

    /// The next slower (`-1`) or faster (`+1`) rate, stopping at the ends.
    pub fn step(self, by: i32) -> Self {
        let i = Self::ALL.iter().position(|r| *r == self).unwrap_or(3) as i32;
        Self::ALL[(i + by).clamp(0, Self::ALL.len() as i32 - 1) as usize]
    }
}

/// The order a row plays its steps in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepDirection {
    #[default]
    Forward,
    Backward,
    PingPong,
    Random,
}

impl StepDirection {
    pub const ALL: [StepDirection; 4] = [Self::Forward, Self::Backward, Self::PingPong, Self::Random];

    pub fn label(self) -> &'static str {
        match self {
            Self::Forward => "Fwd",
            Self::Backward => "Back",
            Self::PingPong => "Ping",
            Self::Random => "Rand",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|d| *d == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// One row: a key and its steps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepRow {
    /// MIDI key 0..=127.
    pub key: u8,
    /// Steps the row plays before it starts over, 1..=[`MAX_ROW_STEPS`].
    pub length: u16,
    /// The steps; one past the end of the list is an off step, so the
    /// list may be shorter than `length` and keeps steps beyond it.
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub rate: StepRate,
    #[serde(default)]
    pub direction: StepDirection,
    /// Delay of every second step, 0..=1 (1 = half a step).
    #[serde(default)]
    pub swing: f32,
}

impl StepRow {
    pub fn new(key: u8) -> Self {
        Self { key, length: DEFAULT_ROW_STEPS, steps: Vec::new(), rate: StepRate::default(), direction: StepDirection::default(), swing: 0.0 }
    }

    /// Step `i` (off when the row never set it).
    pub fn step(&self, i: usize) -> Step {
        self.steps.get(i).copied().unwrap_or_default()
    }

    /// Change step `i`, growing the list to reach it. `false` past
    /// [`MAX_ROW_STEPS`].
    pub fn edit_step(&mut self, i: usize, f: impl FnOnce(&mut Step)) -> bool {
        if i >= usize::from(MAX_ROW_STEPS) {
            return false;
        }
        if self.steps.len() <= i {
            self.steps.resize(i + 1, Step::default());
        }
        f(&mut self.steps[i]);
        true
    }

    /// Which step plays in slot `slot` (0, 1, 2… from the pattern's start).
    pub fn step_index(&self, slot: u64) -> usize {
        let len = u64::from(self.length.clamp(1, MAX_ROW_STEPS));
        match self.direction {
            StepDirection::Forward => (slot % len) as usize,
            StepDirection::Backward => (len - 1 - slot % len) as usize,
            StepDirection::PingPong if len > 1 => {
                let period = 2 * len - 2;
                let p = slot % period;
                (if p < len { p } else { period - p }) as usize
            }
            StepDirection::PingPong => 0,
            // Seeded by the key and the slot: the same every time it is rendered.
            StepDirection::Random => (mix(u64::from(self.key) << 32 ^ slot) % len) as usize,
        }
    }
}

/// splitmix64.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The pattern of a step clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepPattern {
    pub rows: Vec<StepRow>,
    /// Length the rows repeat over, in ticks; the clip is as long (and
    /// loops over it when stretched).
    pub span: Tick,
}

impl StepPattern {
    /// A pattern of `span` ticks with an empty row per key.
    pub fn new(keys: &[u8], span: Tick) -> Self {
        Self { rows: keys.iter().take(MAX_ROWS).map(|k| StepRow::new(*k)).collect(), span: span.max(PPQ / 8) }
    }

    pub fn row_of(&self, key: u8) -> Option<usize> {
        self.rows.iter().position(|r| r.key == key)
    }
}

/// A hit of the rendered pattern, before it gets a note id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub key: u8,
    pub start: Tick,
    pub length: Tick,
    pub velocity: f32,
    pub chance: f32,
}

/// The hits `pattern` plays over `0..span`, sorted by start then key.
/// Deterministic: the random direction depends only on the key and the
/// position, and the chance is left to the player (it is the note's
/// `probability`).
pub fn render(pattern: &StepPattern) -> Vec<Hit> {
    let mut out = Vec::new();
    for row in &pattern.rows {
        let step_ticks = row.rate.ticks();
        let swing = f64::from(row.swing.clamp(0.0, 1.0));
        let mut slot = 0u64;
        loop {
            let base = slot as i64 * step_ticks;
            let start = base + if slot % 2 == 1 { (swing * step_ticks as f64 / 2.0).round() as Tick } else { 0 };
            if base >= pattern.span {
                break;
            }
            let step = row.step(row.step_index(slot));
            if step.on && start < pattern.span {
                let hits = Tick::from(step.repeat.clamp(1, MAX_REPEAT));
                let sub = step_ticks / hits;
                for h in 0..hits {
                    let at = start + h * sub;
                    if at >= pattern.span {
                        break;
                    }
                    let length = ((sub as f64 * GATE).round() as Tick).max(1).min(pattern.span - at);
                    out.push(Hit { key: row.key, start: at, length, velocity: step.velocity.clamp(0.01, 1.0), chance: step.chance.clamp(0.0, 1.0) });
                }
            }
            slot += 1;
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.key.cmp(&b.key)));
    out
}

/// [`render`] as notes with fresh ids.
pub fn render_notes(p: &mut Project, pattern: &StepPattern) -> Vec<Note> {
    render(pattern)
        .into_iter()
        .map(|h| {
            let mut n = Note::new(p.alloc_note_id(), h.key, h.start, h.length, h.velocity);
            n.probability = h.chance;
            n
        })
        .collect()
}

/// The pattern a notes source plays as steps at `rate`: one row per key (high
/// to low), each note snapped to the nearest step, rows as long as `span`
/// at that rate. A step keeps the loudest note that landed on it; notes
/// outside the span are dropped, the steps never go past [`MAX_ROW_STEPS`]
/// (a longer span is cut), and keys past the [`MAX_ROWS`] highest are dropped.
pub fn pattern_from_notes(notes: &[Note], span: Tick, rate: StepRate) -> StepPattern {
    let step_ticks = rate.ticks();
    let length = ((span + step_ticks - 1) / step_ticks).clamp(1, Tick::from(MAX_ROW_STEPS)) as u16;
    let mut keys: Vec<u8> = notes.iter().filter(|n| !n.muted).map(|n| n.key).collect();
    keys.sort_unstable_by(|a, b| b.cmp(a));
    keys.dedup();
    keys.truncate(MAX_ROWS);
    let mut pattern = StepPattern { rows: keys.iter().map(|k| StepRow { length, rate, ..StepRow::new(*k) }).collect(), span };
    for n in notes.iter().filter(|n| !n.muted) {
        if n.start >= span {
            continue;
        }
        // Rounded to the nearest step; the last half-step still belongs to the last one.
        let i = ((n.start as f64 / step_ticks as f64).round() as i64).clamp(0, i64::from(length) - 1) as usize;
        let Some(r) = pattern.row_of(n.key) else { continue };
        pattern.rows[r].edit_step(i, |s| {
            if !s.on || n.velocity > s.velocity {
                *s = Step { on: true, velocity: n.velocity.clamp(0.01, 1.0), chance: n.probability.clamp(0.0, 1.0), repeat: 1 };
            }
        });
    }
    pattern
}

// ── Command builders ──

/// The pattern of the notes source behind `clip`, with the source id.
pub fn clip_pattern(p: &Project, clip: ClipId) -> Option<(SourceId, &StepPattern)> {
    let c = p.clips.get(&clip)?;
    match p.sources.get(&c.source)? {
        Source::Notes(NoteSource { pattern: Some(pat), .. }) => Some((c.source, pat)),
        _ => None,
    }
}

/// A step clip of `span` ticks on `track` at `start`: the commands to add
/// and place it, and the ids of the clip and its placement. `keys` are the
/// rows. `None` when `track` does not exist or is not an instrument track.
pub fn new_step_clip(p: &mut Project, track: TrackId, start: Tick, span: Tick, keys: &[u8], name: &str, color: Color) -> Option<(Vec<Command>, ClipId, PlacementId)> {
    if p.track(track)?.kind != TrackKind::Instrument {
        return None;
    }
    let pattern = StepPattern::new(keys, span);
    let notes = render_notes(p, &pattern);
    let source = p.alloc_source_id();
    let clip = p.alloc_clip_id();
    let placement = p.alloc_placement_id();
    let mut c = Clip::new(name, color, source, pattern.span);
    c.looping = Some(LoopRegion { start: 0, end: pattern.span });
    Some((
        vec![
            Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes, pattern: Some(pattern) })) },
            Command::AddClip { id: clip, clip: Box::new(c) },
            Command::PlaceClip { track, placement: Placement { id: placement, clip, start: start.max(0), lane: 0 } },
        ],
        clip,
        placement,
    ))
}

/// Replace the pattern of `clip` with `pattern` (the notes follow).
pub fn set_pattern(p: &mut Project, clip: ClipId, pattern: StepPattern) -> Option<Command> {
    let (source, _) = clip_pattern(p, clip)?;
    // Rendered here so redo replays the same notes, ids included.
    let notes = Some(render_notes(p, &pattern));
    Some(Command::SetStepPattern { source, pattern: Some(Box::new(pattern)), notes })
}

/// Edit the pattern of `clip` with `f`; `None` when it is not a step clip or
/// `f` changes nothing.
pub fn edit_pattern(p: &mut Project, clip: ClipId, f: impl FnOnce(&mut StepPattern)) -> Option<Command> {
    let (_, cur) = clip_pattern(p, clip)?;
    let (cur, mut next) = (cur.clone(), cur.clone());
    f(&mut next);
    if next == cur {
        return None;
    }
    set_pattern(p, clip, next)
}

/// Turn a step clip into a plain notes clip: the notes stay as they are.
pub fn convert_to_notes(p: &Project, clip: ClipId) -> Option<Command> {
    let (source, _) = clip_pattern(p, clip)?;
    Some(Command::SetStepPattern { source, pattern: None, notes: None })
}

/// Turn a notes clip into a step clip at `rate`: the notes snap to the steps
/// (see [`pattern_from_notes`]) and the clip loops over its length.
/// `None` for an audio clip, a clip that is a step clip already, or one that
/// does not start at 0 (the editors show a clip's notes from 0).
pub fn convert_to_steps(p: &mut Project, clip: ClipId, rate: StepRate) -> Option<Vec<Command>> {
    let c = p.clips.get(&clip)?;
    let Some(Source::Notes(src)) = p.sources.get(&c.source) else { return None };
    if src.pattern.is_some() || c.offset != 0 {
        return None;
    }
    let pattern = pattern_from_notes(&src.notes, c.length.max(1), rate);
    let (source, c) = (c.source, c.clone());
    let notes = Some(render_notes(p, &pattern));
    let mut new = c.clone();
    if new.looping.is_none() {
        new.looping = Some(LoopRegion { start: 0, end: pattern.span });
    }
    let mut cmds = vec![Command::SetStepPattern { source, pattern: Some(Box::new(pattern)), notes }];
    if new != c {
        cmds.push(Command::UpdateClip { id: clip, clip: Box::new(new) });
    }
    Some(cmds)
}

/// Switch step `i` of row `row` on or off.
pub fn toggle_step(p: &mut Project, clip: ClipId, row: usize, i: usize) -> Option<Command> {
    edit_pattern(p, clip, |pat| {
        if let Some(r) = pat.rows.get_mut(row) {
            r.edit_step(i, |s| s.on = !s.on);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(rows: Vec<StepRow>) -> StepPattern {
        StepPattern { rows, span: 4 * PPQ }
    }

    fn row_with(key: u8, on: &[usize]) -> StepRow {
        let mut r = StepRow::new(key);
        for i in on {
            r.edit_step(*i, |s| s.on = true);
        }
        r
    }

    #[test]
    fn a_note_in_the_last_half_step_lands_on_the_last_step() {
        let note = Note::new(NoteId(1), 36, 4 * PPQ - PPQ / 8, PPQ / 8, 0.8);
        let pat = pattern_from_notes(&[note], 4 * PPQ, StepRate::Sixteenth);
        assert!(pat.rows[0].steps.last().is_some_and(|s| s.on), "the note is kept, on step 16");
    }

    #[test]
    fn renders_steps_at_their_rate() {
        let hits = render(&pat(vec![row_with(36, &[0, 4, 8, 12])]));
        assert_eq!(hits.iter().map(|h| h.start).collect::<Vec<_>>(), vec![0, PPQ, 2 * PPQ, 3 * PPQ]);
        assert!(hits.iter().all(|h| h.length > 0 && h.length < PPQ / 4));
    }

    #[test]
    fn rows_of_different_length_drift_apart() {
        let mut a = row_with(36, &[0]);
        a.length = 16;
        let mut b = row_with(38, &[0]);
        b.length = 12;
        let hits = render(&pat(vec![a, b]));
        let b_starts: Vec<Tick> = hits.iter().filter(|h| h.key == 38).map(|h| h.start).collect();
        // A 12-step row restarts every 12 sixteenths inside a 16-sixteenth bar.
        assert_eq!(b_starts, vec![0, 12 * PPQ / 4]);
        assert_eq!(hits.iter().filter(|h| h.key == 36).count(), 1);
    }

    #[test]
    fn direction_orders_the_steps() {
        let mut r = StepRow::new(60);
        r.length = 4;
        let order = |d: StepDirection, n: u64| {
            let mut r = r.clone();
            r.direction = d;
            (0..n).map(|s| r.step_index(s)).collect::<Vec<_>>()
        };
        assert_eq!(order(StepDirection::Forward, 6), vec![0, 1, 2, 3, 0, 1]);
        assert_eq!(order(StepDirection::Backward, 5), vec![3, 2, 1, 0, 3]);
        assert_eq!(order(StepDirection::PingPong, 8), vec![0, 1, 2, 3, 2, 1, 0, 1]);
        assert_eq!(order(StepDirection::Random, 32), order(StepDirection::Random, 32));
        assert!(order(StepDirection::Random, 32).iter().all(|i| *i < 4));
    }

    #[test]
    fn swing_delays_the_odd_steps_only() {
        let mut r = row_with(36, &[0, 1, 2, 3]);
        r.swing = 1.0;
        let starts: Vec<Tick> = render(&pat(vec![r])).iter().map(|h| h.start).collect();
        let s = PPQ / 4;
        assert_eq!(starts, vec![0, s + s / 2, 2 * s, 3 * s + s / 2]);
    }

    #[test]
    fn repeat_splits_the_step_and_chance_goes_to_the_hit() {
        let mut r = StepRow::new(42);
        r.edit_step(0, |s| {
            s.on = true;
            s.repeat = 3;
            s.chance = 0.5;
        });
        let hits = render(&pat(vec![r]));
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[1].start, PPQ / 12);
        assert!(hits.iter().all(|h| h.chance == 0.5));
    }

    #[test]
    fn nothing_plays_past_the_span() {
        let mut r = row_with(36, &[0, 1, 2, 3, 4, 5, 6, 7]);
        r.swing = 1.0;
        let mut p = pat(vec![r]);
        p.span = PPQ / 2 + 10;
        assert!(render(&p).iter().all(|h| h.start + h.length <= p.span));
    }

    #[test]
    fn notes_snap_to_steps_and_back() {
        let notes = vec![
            Note::new(NoteId(1), 38, PPQ, 100, 0.9),
            Note::new(NoteId(2), 36, 0, 100, 0.7),
            Note::new(NoteId(3), 36, 2 * PPQ + 10, 100, 0.5),
        ];
        let pat = pattern_from_notes(&notes, 4 * PPQ, StepRate::Sixteenth);
        assert_eq!(pat.rows.iter().map(|r| r.key).collect::<Vec<_>>(), vec![38, 36]);
        assert_eq!(pat.rows[0].length, 16);
        assert!(pat.rows[0].step(4).on && pat.rows[1].step(0).on && pat.rows[1].step(8).on);
        let hits = render(&pat);
        assert_eq!(hits.iter().map(|h| (h.key, h.start)).collect::<Vec<_>>(), vec![(36, 0), (38, PPQ), (36, 2 * PPQ)]);
    }

    #[test]
    fn old_rows_load_with_defaults() {
        let row: StepRow = serde_json::from_str(r#"{"key":36,"length":8}"#).unwrap();
        assert_eq!(row.rate, StepRate::Sixteenth);
        assert!(!row.step(3).on);
    }

    use crate::history::{History, Origin};

    /// A project with an instrument track holding a step clip of two rows.
    fn with_clip() -> (Project, History, ClipId) {
        let mut p = Project::new("t");
        let track = p.alloc_track_id();
        p.tracks.push(Track::new(track, TrackKind::Instrument, "T", Color(0)));
        let (cmds, clip, _) = new_step_clip(&mut p, track, 0, 4 * PPQ, &[36, 38], "Steps", Color(0)).unwrap();
        let mut h = History::default();
        h.apply(&mut p, "Create", Origin::User, cmds).unwrap();
        (p, h, clip)
    }

    fn notes_of(p: &Project, clip: ClipId) -> Vec<Note> {
        match p.sources.get(&p.clips[&clip].source) {
            Some(Source::Notes(n)) => n.notes.clone(),
            _ => panic!("not a notes clip"),
        }
    }

    #[test]
    fn editing_steps_renders_the_notes_and_undoes() {
        let (mut p, mut h, clip) = with_clip();
        assert!(notes_of(&p, clip).is_empty());
        for i in [0, 4] {
            let c = toggle_step(&mut p, clip, 0, i).unwrap();
            h.apply(&mut p, "Toggle", Origin::User, vec![c]).unwrap();
        }
        let c = edit_pattern(&mut p, clip, |pat| {
            pat.rows[0].edit_step(4, |s| {
                s.chance = 0.25;
                s.repeat = 2;
            });
        })
        .unwrap();
        h.apply(&mut p, "Edit", Origin::User, vec![c]).unwrap();
        let notes = notes_of(&p, clip);
        assert_eq!(notes.iter().map(|n| (n.key, n.start)).collect::<Vec<_>>(), vec![(36, 0), (36, PPQ), (36, PPQ + PPQ / 8)]);
        assert_eq!(notes[1].probability, 0.25);
        let ids: Vec<NoteId> = notes.iter().map(|n| n.id).collect();
        h.undo(&mut p).unwrap();
        assert_eq!(notes_of(&p, clip).len(), 2);
        h.redo(&mut p).unwrap();
        assert_eq!(notes_of(&p, clip).iter().map(|n| n.id).collect::<Vec<_>>(), ids, "redo brings the same notes back");
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert!(notes_of(&p, clip).is_empty());
        assert!(clip_pattern(&p, clip).is_some());
    }

    #[test]
    fn the_note_commands_leave_a_step_clip_alone_until_it_is_converted() {
        let (mut p, mut h, clip) = with_clip();
        let src = p.clips[&clip].source;
        let c = toggle_step(&mut p, clip, 1, 2).unwrap();
        h.apply(&mut p, "Toggle", Origin::User, vec![c]).unwrap();
        let add = Command::AddNotes { source: src, notes: vec![Note::new(p.alloc_note_id(), 60, 0, 10, 1.0)] };
        assert!(h.apply(&mut p, "Add", Origin::User, vec![add.clone()]).is_err());
        let before = notes_of(&p, clip);
        let c = convert_to_notes(&p, clip).unwrap();
        h.apply(&mut p, "Convert", Origin::User, vec![c]).unwrap();
        assert!(clip_pattern(&p, clip).is_none());
        assert_eq!(notes_of(&p, clip), before, "the notes stay");
        h.apply(&mut p, "Add", Origin::User, vec![add]).unwrap();
        assert_eq!(notes_of(&p, clip).len(), 2);
        // Undo the add and the conversion: the pattern is back, with its notes.
        h.undo(&mut p).unwrap();
        h.undo(&mut p).unwrap();
        assert!(clip_pattern(&p, clip).is_some());
        assert_eq!(notes_of(&p, clip), before);
    }

    #[test]
    fn notes_convert_to_steps_and_the_project_round_trips() {
        let (mut p, mut h, clip) = with_clip();
        let c = convert_to_notes(&p, clip).unwrap();
        h.apply(&mut p, "Convert", Origin::User, vec![c]).unwrap();
        assert!(convert_to_steps(&mut p, clip, StepRate::Sixteenth).is_some(), "a notes clip converts back");
        let src = p.clips[&clip].source;
        let add = Command::AddNotes { source: src, notes: vec![Note::new(p.alloc_note_id(), 42, 3 * PPQ / 4 + 7, 100, 0.6)] };
        h.apply(&mut p, "Add", Origin::User, vec![add]).unwrap();
        let cmds = convert_to_steps(&mut p, clip, StepRate::Sixteenth).unwrap();
        h.apply(&mut p, "To steps", Origin::User, cmds).unwrap();
        let (_, pat) = clip_pattern(&p, clip).unwrap();
        assert!(pat.rows[0].step(3).on);
        assert_eq!(notes_of(&p, clip)[0].start, 3 * PPQ / 4, "snapped to the grid");
        let json = p.to_json().unwrap();
        let back = Project::open_json(&json).unwrap();
        assert_eq!(back.sources, p.sources);
    }

    #[test]
    fn a_project_without_patterns_serialises_without_the_field() {
        let mut p = Project::new("t");
        let s = p.alloc_source_id();
        Command::AddSource { id: s, source: Box::new(Source::Notes(NoteSource::default())) }.apply(&mut p).unwrap();
        assert!(!p.to_json().unwrap().contains("pattern"));
        let old: NoteSource = serde_json::from_str(r#"{"notes":[]}"#).unwrap();
        assert!(old.pattern.is_none());
    }
}
