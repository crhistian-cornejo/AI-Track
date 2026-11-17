//! Grooves, as in Ableton Live's Groove Pool.
//!
//! A [`Groove`] is a template laid over a grid: for every cell of a repeating
//! pattern it says how much later (or earlier) a note on that cell sounds, how
//! much louder or softer, and how much longer or shorter. A clip points at a
//! groove from the project's pool; the notes stay on their grid in the
//! clip and the engine moves them when it plays them. [`bake`] writes the
//! displacement into the notes instead (Live's *Commit*).
//!
//! # The maths (one function, used by the engine and by [`bake`])
//!
//! For a note that starts at `start` ticks (in the source, the grid starts at
//! tick 0) the nearest grid line is cell `c = round(start / base)`; the groove
//! slot is `slots[c mod cells]`. With the groove's [`GrooveSettings`] and the
//! project's global amount `a` (0 to 1.3, [`MAX_GROOVE_AMOUNT`]):
//!
//! ```text
//! shift   = pull × clamp(quantize,0,1) × clamp(a,0,1)       // toward the grid line
//!         + slot.timing × base × timing × a                 // the groove itself
//!         + rand × ½ × base × random × a                    // rand ∈ [-1, 1]
//! velocity' = velocity × (1 + slot.velocity × velocity_amount × a)   // ≥ 1/127
//! length'   = length + slot.length × base × timing × a              // ≥ 1 tick
//! ```
//!
//! `pull` is `c × base − start`: how far the note is from its grid line.
//! `rand` is a hash of the groove, the note's tick **in the clip** and its key
//! ([`random_unit`]): the same every take, in playback and in an export, for
//! any block size, and different in each pass of a loop.
//!
//! # Edges
//!
//! A displaced note never leaves the section of the clip it belongs to (from
//! the clip start, or the loop start, to the clip end or the loop end): its
//! start is held to `[section start, section end − 1 tick]` and its end to the
//! section end. So a note pushed before the clip start plays at the start of
//! the clip, and one pushed past the end plays on the last tick and is cut at
//! the end: nothing hangs.

use crate::command::{Command, CommandError};
use crate::ids::{ClipId, GrooveId};
use crate::project::{Clip, Note, Project, Source};
use crate::time::{Tick, PPQ};
use serde::{Deserialize, Serialize};

/// Highest global groove amount (130 %, as in Live).
pub const MAX_GROOVE_AMOUNT: f32 = 1.3;
/// Most cells a groove can have.
pub const MAX_GROOVE_CELLS: usize = 256;

/// One cell of a groove.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GrooveSlot {
    /// Later (+) or earlier (−), as a fraction of the base cell. −1..=1.
    #[serde(default)]
    pub timing: f32,
    /// Louder (+) or softer (−), as a fraction of the note's velocity. −1..=1.
    #[serde(default)]
    pub velocity: f32,
    /// Longer (+) or shorter (−), as a fraction of the base cell. −1..=1.
    #[serde(default)]
    pub length: f32,
}

/// A groove template: `slots.len()` cells of `base` ticks, repeating.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Groove {
    pub name: String,
    /// The grid cell in ticks: 1/8 = `PPQ / 2`, 1/16 = `PPQ / 4`, a 1/16
    /// triplet = `PPQ / 6`…
    pub base: Tick,
    pub slots: Vec<GrooveSlot>,
}

impl Groove {
    /// A groove of `cells` cells that does nothing yet.
    pub fn new(name: impl Into<String>, base: Tick, cells: usize) -> Self {
        Self { name: name.into(), base: base.max(1), slots: vec![GrooveSlot::default(); cells.clamp(1, MAX_GROOVE_CELLS)] }
    }

    pub fn cells(&self) -> usize {
        self.slots.len().max(1)
    }

    /// Length of the pattern in ticks.
    pub fn length(&self) -> Tick {
        self.base.max(1) * self.cells() as Tick
    }

    /// Length of the pattern in bars of `bar_ticks`.
    pub fn bars(&self, bar_ticks: Tick) -> f64 {
        self.length() as f64 / bar_ticks.max(1) as f64
    }

    /// The slot of cell `cell` (the pattern repeats in both directions).
    pub fn slot(&self, cell: i64) -> GrooveSlot {
        if self.slots.is_empty() {
            return GrooveSlot::default();
        }
        self.slots[cell.rem_euclid(self.slots.len() as i64) as usize]
    }

    /// Bring the values into range: base ≥ 1, 1 to [`MAX_GROOVE_CELLS`]
    /// cells, offsets in −1..=1.
    pub fn normalize(&mut self) {
        self.base = self.base.max(1);
        self.slots.truncate(MAX_GROOVE_CELLS);
        if self.slots.is_empty() {
            self.slots.push(GrooveSlot::default());
        }
        let fix = |v: f32| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
        for s in &mut self.slots {
            *s = GrooveSlot { timing: fix(s.timing), velocity: fix(s.velocity), length: fix(s.length) };
        }
    }

    /// What this groove does to a note that starts at `start` (source ticks):
    /// the terms the settings and the amount then scale (see the module docs).
    pub fn terms(&self, start: Tick) -> NoteGroove {
        let base = self.base.max(1) as f64;
        let cell = (start as f64 / base).round() as i64;
        let slot = self.slot(cell);
        NoteGroove { pull: (cell as f64 * base - start as f64) as f32, timing: slot.timing * self.base.max(1) as f32, velocity: slot.velocity, length: slot.length * self.base.max(1) as f32 }
    }
}

/// How strongly a pool groove acts (Live's Quantize, Timing, Random and
/// Velocity sliders).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GrooveSettings {
    /// 0..=1: how much of the way to the grid line the notes are moved before
    /// the groove is added.
    #[serde(default)]
    pub quantize: f32,
    /// 0..=1.3: scale of the groove's timing (and length) offsets.
    #[serde(default = "one")]
    pub timing: f32,
    /// 0..=1: random timing, up to ± ½ cell at 1.
    #[serde(default)]
    pub random: f32,
    /// −1..=1.3: scale of the velocity offsets (negative inverts them).
    #[serde(default = "one")]
    pub velocity: f32,
}

fn one() -> f32 {
    1.0
}

impl Default for GrooveSettings {
    fn default() -> Self {
        Self { quantize: 0.0, timing: 1.0, random: 0.0, velocity: 1.0 }
    }
}

impl GrooveSettings {
    pub fn normalized(self) -> Self {
        let fix = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
        Self {
            quantize: fix(self.quantize, 0.0, 1.0, 0.0),
            timing: fix(self.timing, 0.0, MAX_GROOVE_AMOUNT, 1.0),
            random: fix(self.random, 0.0, 1.0, 0.0),
            velocity: fix(self.velocity, -1.0, MAX_GROOVE_AMOUNT, 1.0),
        }
    }
}

/// A groove in the project's pool, with its settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoolGroove {
    pub id: GrooveId,
    pub groove: Groove,
    #[serde(default)]
    pub settings: GrooveSettings,
}

/// The raw terms of a groove for one note (before settings and amount).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NoteGroove {
    /// Ticks to the nearest grid line (the move a full Quantize makes).
    pub pull: f32,
    /// Ticks the slot moves the note at Timing 100 %.
    pub timing: f32,
    /// Relative velocity change at Velocity 100 %.
    pub velocity: f32,
    /// Ticks the slot adds to the length at Timing 100 %.
    pub length: f32,
}

/// The settings of a groove with the global amount, ready to apply.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrooveParams {
    pub quantize: f32,
    pub timing: f32,
    pub velocity: f32,
    pub random: f32,
    /// The project's global groove amount, 0..=[`MAX_GROOVE_AMOUNT`].
    pub amount: f32,
}

impl GrooveParams {
    pub fn new(settings: &GrooveSettings, amount: f32) -> Self {
        let s = settings.normalized();
        Self { quantize: s.quantize, timing: s.timing, velocity: s.velocity, random: s.random, amount: if amount.is_finite() { amount.clamp(0.0, MAX_GROOVE_AMOUNT) } else { 1.0 } }
    }

    /// Ticks to add to the note's start (`rand` in −1..=1, see [`random_unit`]).
    #[inline]
    pub fn shift(&self, t: &NoteGroove, base: Tick, rand: f32) -> f64 {
        let a = self.amount as f64;
        t.pull as f64 * self.quantize as f64 * a.clamp(0.0, 1.0) + t.timing as f64 * self.timing as f64 * a + rand as f64 * 0.5 * base.max(1) as f64 * self.random as f64 * a
    }

    /// Factor for the velocity, never below one MIDI step.
    #[inline]
    pub fn velocity_factor(&self, t: &NoteGroove) -> f32 {
        (1.0 + t.velocity * self.velocity * self.amount).max(0.0)
    }

    /// Ticks to add to the length.
    #[inline]
    pub fn length_delta(&self, t: &NoteGroove) -> f64 {
        t.length as f64 * self.timing as f64 * self.amount as f64
    }
}

/// A velocity after the groove: scaled, within one MIDI step and 1. A note
/// with velocity 0 stays silent.
#[inline]
pub fn grooved_velocity(velocity: f32, factor: f32) -> f32 {
    if velocity <= 0.0 { 0.0 } else { (velocity * factor).clamp(1.0 / 127.0, 1.0) }
}

/// A repeatable pseudo-random number in −1..=1 from the groove, the note's
/// tick in the clip and its key (splitmix64: no state, no allocation).
pub fn random_unit(seed: u64, tick: Tick, key: u8) -> f32 {
    let mut z = seed ^ (tick as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ((key as u64) << 56);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
}

/// The note's start, length and velocity with the groove applied, in whole
/// ticks, held inside `[lo, hi)` (the section of the clip, in source ticks;
/// `clip_origin` is the clip's first source tick, which the random is
/// measured from). What [`bake`] writes into notes.
pub fn grooved(note: &Note, groove: &Groove, params: &GrooveParams, seed: u64, lo: Tick, hi: Tick, clip_origin: Tick) -> Note {
    let t = groove.terms(note.start);
    let rand = if params.random > 0.0 { random_unit(seed, note.start - clip_origin, note.key) } else { 0.0 };
    let start = ((note.start as f64 + params.shift(&t, groove.base, rand)).round() as Tick).clamp(lo, (hi - 1).max(lo));
    let wanted = if note.length > 0 { ((note.length as f64 + params.length_delta(&t)).round() as Tick).max(1) } else { 0 };
    let end = (start + wanted).min(hi.max(start));
    Note { start, length: (end - start).max(0), velocity: grooved_velocity(note.velocity, params.velocity_factor(&t)), ..note.clone() }
}

/// Commit: the groove written into the notes whose start is in `[lo, hi)`.
/// Returns the notes that change (same ids), for `Command::UpdateNotes`.
pub fn bake(notes: &[Note], groove: &PoolGroove, amount: f32, lo: Tick, hi: Tick) -> Vec<Note> {
    let params = GrooveParams::new(&groove.settings, amount);
    let seed = groove.id.0;
    let mut g = groove.groove.clone();
    g.normalize();
    notes
        .iter()
        .filter(|n| n.start >= lo && n.start < hi)
        .map(|n| grooved(n, &g, &params, seed, lo, hi, lo))
        .filter(|n2| notes.iter().find(|n| n.id == n2.id).is_some_and(|n| n != n2))
        .collect()
}

/// Read a groove out of notes: for each cell of a pattern of `cells` cells of
/// `base` ticks, the average timing, velocity (relative to the average of all
/// the notes) and length (relative to the average length, in cells) of the
/// notes that fall on it. Notes whose start is in `[lo, hi)` count; cells with
/// no note stay neutral. `None` without notes or with an invalid grid.
pub fn extract(notes: &[Note], lo: Tick, hi: Tick, base: Tick, cells: usize, name: &str) -> Option<Groove> {
    if base < 1 || cells == 0 || cells > MAX_GROOVE_CELLS {
        return None;
    }
    let picked: Vec<&Note> = notes.iter().filter(|n| !n.muted && n.start >= lo && n.start < hi).collect();
    if picked.is_empty() {
        return None;
    }
    let mean_vel = picked.iter().map(|n| n.velocity as f64).sum::<f64>() / picked.len() as f64;
    let mean_len = picked.iter().map(|n| n.length as f64).sum::<f64>() / picked.len() as f64;
    let mut acc = vec![(0.0f64, 0.0f64, 0.0f64, 0usize); cells];
    for n in picked {
        let cell = (n.start as f64 / base as f64).round() as i64;
        let a = &mut acc[cell.rem_euclid(cells as i64) as usize];
        a.0 += (n.start as f64 - cell as f64 * base as f64) / base as f64;
        a.1 += if mean_vel > 0.0 { n.velocity as f64 / mean_vel - 1.0 } else { 0.0 };
        a.2 += (n.length as f64 - mean_len) / base as f64;
        a.3 += 1;
    }
    let mut g = Groove::new(name, base, cells);
    for (slot, a) in g.slots.iter_mut().zip(acc) {
        if a.3 > 0 {
            let n = a.3 as f64;
            *slot = GrooveSlot { timing: (a.0 / n) as f32, velocity: (a.1 / n) as f32, length: (a.2 / n) as f32 };
        }
    }
    g.normalize();
    Some(g)
}

// ───────────────────────────── Clips ─────────────────────────────

/// The source ticks a notes clip plays first: from its offset to the loop end
/// when it loops, else its length. Notes starting here are the ones a groove
/// moves; the clip's first pass is what [`commit_clip`] bakes.
pub fn clip_region(clip: &Clip) -> (Tick, Tick) {
    match clip.looping.filter(|l| l.end > l.start) {
        Some(l) => (clip.offset.min(l.end), l.end),
        None => (clip.offset, clip.offset + clip.length.max(0)),
    }
}

/// Commit (Live's *Commit Groove*): the commands that write the clip's groove
/// into its notes and set the clip back to no groove (`SetClipGroove`), one undo step
/// (`Command::Batch`, whose inverse restores both exactly). `None` when the
/// clip has no groove, is not a notes clip, or the groove changes nothing.
///
/// The notes of the source that start inside the clip's first pass
/// ([`clip_region`]) are rewritten; another clip sharing the source sees them
/// too. Uses the project's global amount, so a groove at 0 % commits nothing.
pub fn commit_clip(p: &Project, clip: ClipId) -> Result<Option<Command>, CommandError> {
    let c = p.clips.get(&clip).ok_or(CommandError::ClipNotFound(clip))?;
    let Some(entry) = p.clip_groove(clip) else { return Ok(None) };
    let Some(Source::Notes(src)) = p.sources.get(&c.source) else { return Ok(None) };
    let (lo, hi) = clip_region(c);
    let notes = bake(&src.notes, entry, p.groove_amount, lo, hi);
    let mut commands = Vec::new();
    if !notes.is_empty() {
        commands.push(Command::UpdateNotes { source: c.source, notes });
    }
    commands.push(Command::SetClipGroove { clip, groove: None });
    Ok(Some(Command::Batch { commands }))
}

/// Extract a groove from a notes clip (Live's *Extract Groove(s)*): the
/// average timing, velocity and length of its notes on a pattern of `cells`
/// cells of `base` ticks. `None` for an audio clip (reading a groove from
/// audio needs transient detection, which is not done here), a clip
/// without notes, or an invalid grid. The result is not in the pool until
/// the caller adds it (`Command::AddGroove`).
pub fn extract_clip(p: &Project, clip: ClipId, base: Tick, cells: usize, name: &str) -> Option<Groove> {
    let c = p.clips.get(&clip)?;
    let Source::Notes(src) = p.sources.get(&c.source)? else { return None };
    let (lo, hi) = clip_region(c);
    extract(&src.notes, lo, hi, base, cells, name)
}

// ───────────────────────────── Factory grooves ─────────────────────────────

/// A pair of cells where the second is late by `swing` (50 % = straight,
/// 66.7 % = triplet feel), as the offset of the odd cells.
fn swing_offset(percent: f32) -> f32 {
    (2.0 * percent / 100.0 - 1.0).clamp(-1.0, 1.0)
}

fn swung(name: &str, base: Tick, cells: usize, percent: f32) -> Groove {
    let mut g = Groove::new(name, base, cells);
    for (i, s) in g.slots.iter_mut().enumerate() {
        if i % 2 == 1 {
            s.timing = swing_offset(percent);
        }
    }
    g
}

/// A set of grooves for house and techno: 1/16 and 1/8 swing in several
/// degrees, shuffle, laid-back, push and accented bounces. Cells are of one
/// bar at most.
pub fn factory_grooves() -> Vec<Groove> {
    let s16 = PPQ / 4;
    let s8 = PPQ / 2;
    let mut v = Vec::new();
    for p in [54.0f32, 58.0, 62.0, 66.0, 70.0] {
        v.push(swung(&format!("16th Swing {p:.0}%"), s16, 16, p));
    }
    for p in [54.0f32, 58.0, 62.0, 66.0] {
        v.push(swung(&format!("8th Swing {p:.0}%"), s8, 8, p));
    }
    // Shuffle: the triplet feel on 16ths with the off-beats a touch softer.
    let mut shuffle = swung("16th Shuffle", s16, 16, 66.7);
    for (i, s) in shuffle.slots.iter_mut().enumerate() {
        if i % 2 == 1 {
            s.velocity = -0.2;
        }
    }
    v.push(shuffle);
    // Laid back: the backbeat (beats 2 and 4) and the off-beats a little late.
    let mut laid = Groove::new("Laid Back", s16, 16);
    for (i, s) in laid.slots.iter_mut().enumerate() {
        s.timing = match i {
            4 | 12 => 0.22,
            i if i % 4 == 2 => 0.10,
            i if i % 2 == 1 => 0.06,
            _ => 0.0,
        };
    }
    v.push(laid);
    // Push: everything but the downbeat of each beat a little ahead of the grid.
    let mut push = Groove::new("Push", s16, 16);
    for (i, s) in push.slots.iter_mut().enumerate() {
        s.timing = if i % 4 == 0 { 0.0 } else { -0.08 };
        s.velocity = if i % 4 == 0 { 0.1 } else { 0.0 };
    }
    v.push(push);
    // House bounce: 58 % swing with the off-beat eighths accented.
    let mut house = swung("House Bounce", s16, 16, 58.0);
    for (i, s) in house.slots.iter_mut().enumerate() {
        s.velocity = match i % 4 {
            2 => 0.25,
            1 | 3 => -0.2,
            _ => 0.0,
        };
    }
    v.push(house);
    // Techno drive: light swing, hard off-beat, ghosted sixteenths.
    let mut techno = swung("Techno Drive", s16, 16, 54.0);
    for (i, s) in techno.slots.iter_mut().enumerate() {
        s.velocity = match i % 4 {
            0 => 0.1,
            2 => 0.2,
            _ => -0.3,
        };
    }
    v.push(techno);
    // Hat shuffle: a two-bar-feel velocity ramp over a 62 % swing.
    let mut hats = swung("Hat Shuffle", s16, 16, 62.0);
    for (i, s) in hats.slots.iter_mut().enumerate() {
        s.velocity = -0.25 + 0.5 * ((i % 8) as f32 / 7.0);
    }
    v.push(hats);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::NoteId;

    fn note(id: u64, key: u8, start: Tick, length: Tick, velocity: f32) -> Note {
        Note::new(NoteId(id), key, start, length, velocity)
    }

    const S16: Tick = PPQ / 4;

    #[test]
    fn a_swing_pushes_the_odd_cells_by_the_swing_percentage() {
        // 66.7 % is the triplet: the second 16th of a pair a third of a cell late.
        let g = swung("t", S16, 16, 66.7);
        assert!((g.slots[1].timing - 0.3333).abs() < 1e-3 && g.slots[0].timing == 0.0);
        assert_eq!(swing_offset(50.0), 0.0);
        let p = GrooveParams::new(&GrooveSettings::default(), 1.0);
        let t = g.terms(S16); // a note on cell 1
        assert!((p.shift(&t, g.base, 0.0) - 0.3333 * S16 as f64).abs() < 0.5);
        // The pattern repeats: cell 17 behaves as cell 1; so does cell -15.
        assert_eq!(g.terms(17 * S16).timing, t.timing);
        assert_eq!(g.terms(-15 * S16).timing, t.timing);
    }

    #[test]
    fn quantize_pulls_toward_the_grid_and_the_amount_scales_everything() {
        let g = Groove::new("flat", S16, 4);
        let sloppy = note(1, 60, S16 + 40, S16, 0.8);
        let t = g.terms(sloppy.start);
        assert!((t.pull + 40.0).abs() < 1e-3);
        let mut s = GrooveSettings { quantize: 0.5, ..GrooveSettings::default() };
        assert!((GrooveParams::new(&s, 1.0).shift(&t, g.base, 0.0) + 20.0).abs() < 1e-6);
        assert_eq!(GrooveParams::new(&s, 0.0).shift(&t, g.base, 0.0), 0.0, "amount 0 bypasses it all");
        s.quantize = 1.0;
        assert!((GrooveParams::new(&s, 1.3).shift(&t, g.base, 0.0) + 40.0).abs() < 1e-6, "quantize never overshoots");
    }

    #[test]
    fn velocity_and_length_follow_the_slot() {
        let mut g = Groove::new("v", S16, 2);
        g.slots[1] = GrooveSlot { timing: 0.0, velocity: -0.5, length: -0.5 };
        let n = note(1, 60, S16, S16, 0.8);
        let r = grooved(&n, &g, &GrooveParams::new(&GrooveSettings::default(), 1.0), 1, 0, 10 * S16, 0);
        assert!((r.velocity - 0.4).abs() < 1e-6);
        assert_eq!(r.length, S16 / 2);
        // Inverted velocity setting makes it louder; amount 0 changes nothing.
        let inv = GrooveSettings { velocity: -1.0, ..GrooveSettings::default() };
        assert!((grooved(&n, &g, &GrooveParams::new(&inv, 1.0), 1, 0, 10 * S16, 0).velocity - 1.0).abs() < 1e-6);
        assert_eq!(grooved(&n, &g, &GrooveParams::new(&GrooveSettings::default(), 0.0), 1, 0, 10 * S16, 0), n);
        // A silent note stays silent, a soft one never drops below one step.
        assert_eq!(grooved_velocity(0.0, 2.0), 0.0);
        assert_eq!(grooved_velocity(0.5, 0.0), 1.0 / 127.0);
    }

    #[test]
    fn displaced_notes_stay_inside_their_section() {
        let mut g = Groove::new("e", S16, 2);
        g.slots[0].timing = -1.0;
        g.slots[1].timing = 1.0;
        let p = GrooveParams::new(&GrooveSettings::default(), 1.0);
        // Pushed before the clip start: plays on the first tick.
        let early = grooved(&note(1, 60, 0, S16, 1.0), &g, &p, 1, 0, 4 * S16, 0);
        assert_eq!((early.start, early.length), (0, S16));
        // Pushed past the end: the last tick, cut at the end.
        let late = grooved(&note(2, 60, 3 * S16, 3 * S16 / 2, 1.0), &g, &p, 1, 0, 4 * S16, 0);
        assert_eq!(late.start, 4 * S16 - 1);
        assert_eq!(late.start + late.length, 4 * S16, "ends where the section ends");
    }

    #[test]
    fn random_is_a_function_of_position_only() {
        assert_eq!(random_unit(5, 1234, 60), random_unit(5, 1234, 60));
        assert_ne!(random_unit(5, 1234, 60), random_unit(5, 1235, 60));
        assert_ne!(random_unit(5, 1234, 60), random_unit(6, 1234, 60));
        let v: Vec<f32> = (0..2000).map(|t| random_unit(9, t * 7, 36)).collect();
        assert!(v.iter().all(|r| (-1.0..=1.0).contains(r)));
        let mean = v.iter().sum::<f32>() / v.len() as f32;
        assert!(mean.abs() < 0.08, "{mean}");
    }

    #[test]
    fn extract_reads_back_what_a_groove_did() {
        // Sixteenths on a 66.7 % swing with the off-beats softer, 4 bars of them.
        let g = {
            let mut g = swung("s", S16, 16, 66.7);
            for (i, s) in g.slots.iter_mut().enumerate() {
                s.velocity = if i % 2 == 1 { -0.3 } else { 0.0 };
            }
            g
        };
        let p = GrooveParams::new(&GrooveSettings::default(), 1.0);
        let notes: Vec<Note> = (0..64)
            .map(|i| grooved(&note(i as u64 + 1, 42, i * S16, S16 / 2, 0.8), &g, &p, 1, 0, 64 * S16, 0))
            .collect();
        let got = extract(&notes, 0, 64 * S16, S16, 16, "got").expect("a groove");
        for i in 0..16 {
            let want = if i % 2 == 1 { 0.3333 } else { 0.0 };
            assert!((got.slots[i].timing - want).abs() < 0.01, "cell {i}: {} vs {want}", got.slots[i].timing);
        }
        // Velocity is relative to the mean: even cells above it, odd below.
        assert!(got.slots[0].velocity > 0.1 && got.slots[1].velocity < -0.1);
        assert!(extract(&[], 0, 100, S16, 16, "x").is_none());
        assert!(extract(&notes, 0, 100, 0, 16, "x").is_none());
    }

    #[test]
    fn bake_writes_the_groove_and_only_changes_notes_in_the_region() {
        let g = PoolGroove { id: GrooveId(3), groove: swung("s", S16, 16, 66.7), settings: GrooveSettings::default() };
        let notes = vec![note(1, 60, 0, S16, 1.0), note(2, 60, S16, S16, 1.0), note(3, 60, 20 * S16, S16, 1.0)];
        let out = bake(&notes, &g, 1.0, 0, 16 * S16);
        assert_eq!(out.len(), 1, "only the off-beat moves; the other is on a straight cell, the third is outside");
        assert_eq!(out[0].id, NoteId(2));
        assert_eq!(out[0].start, S16 + (S16 as f32 * 0.3333).round() as Tick);
        // Amount 0 bakes nothing.
        assert!(bake(&notes, &g, 0.0, 0, 16 * S16).is_empty());
    }

    #[test]
    fn factory_grooves_are_valid_and_named() {
        let all = factory_grooves();
        assert!(all.len() >= 12);
        for g in &all {
            let mut n = g.clone();
            n.normalize();
            assert_eq!(&n, g, "{} is already within range", g.name);
            assert!(g.length() <= 4 * PPQ);
        }
        let names: std::collections::HashSet<_> = all.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names.len(), all.len(), "unique names");
    }

    // ───────────── Commands, undo, files ─────────────

    use crate::command::Command;
    use crate::history::{History, Origin};
    use crate::project::{Clip, NoteSource, Source, Color};
    use crate::ids::{ClipId, SourceId};

    /// A project with a notes clip of off-beat sixteenths and a pool of one groove.
    fn rig() -> (Project, ClipId, SourceId, GrooveId) {
        let mut p = Project::new("g");
        let sid = p.alloc_source_id();
        let mut src = NoteSource::default();
        for i in 0..16 {
            let id = p.alloc_note_id();
            src.notes.push(Note::new(id, 42, i * S16, S16 / 2, 0.8));
        }
        p.sources.insert(sid, Source::Notes(src));
        let cid = p.alloc_clip_id();
        p.clips.insert(cid, Clip::new("c", Color(0), sid, 16 * S16));
        let gid = p.alloc_groove_id();
        p.grooves.push(PoolGroove { id: gid, groove: swung("Swing", S16, 16, 62.0), settings: GrooveSettings::default() });
        (p, cid, sid, gid)
    }

    /// Apply, check that the inverse restores the project exactly, and that
    /// applying the command again after the undo gives the same result (redo).
    fn undoable(p: &mut Project, c: Command) {
        let before = p.clone();
        let inverse = c.clone().apply(p).expect("applies");
        let after = p.clone();
        inverse.apply(p).expect("the inverse applies");
        assert_eq!(*p, before, "undo restores the project exactly");
        c.apply(p).unwrap();
        assert_eq!(*p, after, "redo gives the same project");
    }

    #[test]
    fn every_groove_command_undoes_exactly() {
        let (mut p, cid, _, gid) = rig();
        let new_id = p.alloc_groove_id();
        undoable(&mut p, Command::AddGroove { entry: Box::new(PoolGroove { id: new_id, groove: Groove::new("Mine", S16, 8), settings: GrooveSettings { quantize: 0.4, ..GrooveSettings::default() } }), index: 0 });
        undoable(&mut p, Command::UpdateGroove { id: gid, groove: Box::new(swung("Renamed", S16, 8, 70.0)) });
        undoable(&mut p, Command::SetGrooveSettings { id: gid, settings: GrooveSettings { quantize: 0.25, timing: 0.7, random: 0.3, velocity: -0.5 } });
        undoable(&mut p, Command::SetClipGroove { clip: cid, groove: Some(gid) });
        undoable(&mut p, Command::SetGrooveAmount { amount: 0.4 });
        // Removing a groove a clip uses sends the clip back to none, and undo points it back.
        Command::SetClipGroove { clip: cid, groove: Some(gid) }.apply(&mut p).unwrap();
        undoable(&mut p, Command::RemoveGroove { id: gid });
        // Mistakes are errors, not panics.
        assert!(Command::SetClipGroove { clip: cid, groove: Some(GrooveId(999)) }.apply(&mut p).is_err());
        assert!(Command::RemoveGroove { id: GrooveId(999) }.apply(&mut p).is_err());
        assert!(Command::SetClipGroove { clip: ClipId(999), groove: None }.apply(&mut p).is_err());
        assert!(Command::AddGroove { entry: Box::new(p.grooves[0].clone()), index: 0 }.apply(&mut p).is_err(), "no duplicate ids");
    }

    #[test]
    fn values_are_brought_into_range() {
        let (mut p, _, _, gid) = rig();
        Command::SetGrooveAmount { amount: 9.0 }.apply(&mut p).unwrap();
        assert_eq!(p.groove_amount, MAX_GROOVE_AMOUNT);
        Command::SetGrooveAmount { amount: f32::NAN }.apply(&mut p).unwrap();
        assert_eq!(p.groove_amount, 1.0);
        Command::SetGrooveSettings { id: gid, settings: GrooveSettings { quantize: 5.0, timing: -3.0, random: f32::NAN, velocity: 9.0 } }.apply(&mut p).unwrap();
        assert_eq!(p.grooves[0].settings, GrooveSettings { quantize: 1.0, timing: 0.0, random: 0.0, velocity: MAX_GROOVE_AMOUNT });
        let wild = Groove { name: "w".into(), base: -4, slots: vec![GrooveSlot { timing: 9.0, velocity: f32::NAN, length: -9.0 }] };
        Command::UpdateGroove { id: gid, groove: Box::new(wild) }.apply(&mut p).unwrap();
        assert_eq!(p.grooves[0].groove.base, 1);
        assert_eq!(p.grooves[0].groove.slots[0], GrooveSlot { timing: 1.0, velocity: 0.0, length: -1.0 });
    }

    #[test]
    fn dragging_the_amount_or_a_setting_is_one_undo_step() {
        let (mut p, _, _, gid) = rig();
        let mut h = History::default();
        for a in [0.9, 0.8, 0.7, 0.6] {
            h.apply_coalesced(&mut p, "groove-amount", "Groove amount", vec![Command::SetGrooveAmount { amount: a }]).unwrap();
        }
        for q in [0.1, 0.2, 0.3] {
            h.apply_coalesced(&mut p, "groove-quantize", "Quantize", vec![Command::SetGrooveSettings { id: gid, settings: GrooveSettings { quantize: q, ..GrooveSettings::default() } }]).unwrap();
        }
        assert_eq!((p.groove_amount, p.grooves[0].settings.quantize), (0.6, 0.3));
        h.undo(&mut p).unwrap();
        assert_eq!(p.grooves[0].settings.quantize, 0.0);
        h.undo(&mut p).unwrap();
        assert_eq!(p.groove_amount, 1.0);
        let _ = Origin::User;
    }

    #[test]
    fn commit_bakes_the_groove_into_the_notes_in_one_undo_step() {
        let (mut p, cid, sid, gid) = rig();
        Command::SetClipGroove { clip: cid, groove: Some(gid) }.apply(&mut p).unwrap();
        let before = p.clone();
        let commit = commit_clip(&p, cid).unwrap().expect("something to commit");
        let mut h = History::default();
        h.apply(&mut p, "Commit groove", Origin::User, vec![commit]).unwrap();
        assert_eq!(p.clip_groove(cid), None);
        let Source::Notes(n) = &p.sources[&sid] else { panic!() };
        let moved = n.notes.iter().filter(|x| x.start % S16 != 0).count();
        assert_eq!(moved, 8, "every off-beat sixteenth moved: {:?}", n.notes.iter().map(|x| x.start).collect::<Vec<_>>());
        assert_eq!(n.notes[1].start, S16 + (0.24 * S16 as f32).round() as Tick, "62 % swing: a quarter of a cell late");
        // The baked clip no longer needs the groove: committing again finds nothing.
        assert_eq!(commit_clip(&p, cid).unwrap(), None);
        h.undo(&mut p).unwrap();
        assert_eq!(p, before, "one Cmd+Z undoes the commit exactly");
        // No groove, or amount 0: nothing to do.
        let mut none = before.clone();
        none.groove_amount = 0.0;
        assert_eq!(commit_clip(&none, cid).unwrap().map(|c| matches!(c, Command::Batch { commands } if commands.len() == 1)), Some(true), "only the clip's groove link changes");
        assert!(commit_clip(&p, ClipId(999)).is_err());
    }

    #[test]
    fn a_groove_extracted_from_a_clip_plays_that_clip_back() {
        // Notes with a hand-made push: off-beats 12 ticks late, accented.
        let (mut p, cid, sid, _) = rig();
        if let Some(Source::Notes(n)) = p.sources.get_mut(&sid) {
            for (i, note) in n.notes.iter_mut().enumerate() {
                if i % 2 == 1 {
                    note.start += 12;
                    note.velocity = 0.5;
                }
            }
        }
        let g = extract_clip(&p, cid, S16, 16, "Mine").expect("a groove");
        assert!((g.slots[1].timing - 12.0 / S16 as f32).abs() < 1e-4, "{}", g.slots[1].timing);
        assert!(g.slots[1].velocity < 0.0 && g.slots[0].velocity > 0.0);
        // Put on a straight clip it gives the same off-beat positions.
        let params = GrooveParams::new(&GrooveSettings::default(), 1.0);
        let straight = Note::new(NoteId(1), 42, S16, S16 / 2, 0.8);
        assert_eq!(grooved(&straight, &g, &params, 1, 0, 100 * S16, 0).start, S16 + 12);
        // Audio clips are not read.
        let aid = p.alloc_source_id();
        p.sources.insert(aid, Source::Audio(crate::project::AudioSource { path: "x.wav".into(), hash: None, sample_rate: 48_000, channels: 1, frames: 10, warp: Default::default(), key: None }));
        let audio = p.alloc_clip_id();
        p.clips.insert(audio, Clip::new("a", Color(0), aid, 960));
        assert_eq!(extract_clip(&p, audio, S16, 16, "x"), None);
    }

    #[test]
    fn projects_saved_before_grooves_still_open() {
        let (p, cid, _, gid) = rig();
        let mut value = serde_json::to_value(&p).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.remove("grooves");
        obj.remove("groove_amount");
        obj.remove("clip_grooves");
        let old = Project::from_json(&value.to_string()).expect("an old file opens");
        assert!(old.grooves.is_empty());
        assert_eq!(old.groove_amount, 1.0);
        assert_eq!(old.clip_groove(cid), None);
        assert!(old.clip_grooves.is_empty());
        // And a project with grooves round-trips.
        let mut with = p.clone();
        with.clip_grooves.insert(cid, gid);
        with.groove_amount = 0.75;
        assert_eq!(Project::from_json(&serde_json::to_string(&with).unwrap()).unwrap(), with);
    }
}
