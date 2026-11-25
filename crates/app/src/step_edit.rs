//! Editing Step Sequencer clips (Live 13): creating them, changing their
//! pattern and converting them to notes and back. The model owns the rules
//! (`aitrack_model::steps`); every change here is one `Command`.

use crate::state::{AppState, drum_pads};
use aitrack_model::steps::{self, MAX_ROWS, MAX_ROW_STEPS};
use aitrack_model::*;

/// Rows of a new step clip on a track without a drum rack: a triad's notes.
const PITCHED_KEYS: [u8; 3] = [67, 64, 60];

impl AppState {
    /// The pattern of `clip`, when it is a step clip.
    pub fn clip_pattern(&self, clip: ClipId) -> Option<&StepPattern> {
        steps::clip_pattern(&self.project, clip).map(|(_, p)| p)
    }

    /// The keys a new step clip on `track` starts with: the drum rack's pads
    /// (their notes, lowest first), else [`PITCHED_KEYS`].
    pub(crate) fn step_keys(&self, track: TrackId) -> Vec<u8> {
        let mut keys: Vec<u8> = self.project.track(track).map(drum_pads).unwrap_or_default().into_iter().map(|(_, _, k)| k).collect();
        keys.sort_unstable();
        keys.dedup();
        keys.truncate(MAX_ROWS);
        if keys.is_empty() { PITCHED_KEYS.to_vec() } else { keys }
    }

    /// Change the pattern of `clip` with `f` as one undo step. `false` when
    /// the clip is not a step clip or nothing changes.
    pub fn edit_steps(&mut self, clip: ClipId, label: &str, f: impl FnOnce(&mut StepPattern)) -> bool {
        match steps::edit_pattern(&mut self.project, clip, f) {
            Some(c) => {
                self.seal();
                let ok = self.edit(label, vec![c]);
                self.seal();
                ok
            }
            None => false,
        }
    }

    /// [`edit_steps`](Self::edit_steps) for a drag: the frames sharing `key`
    /// are one undo step (end it with `seal`).
    pub fn edit_steps_coalesced(&mut self, clip: ClipId, key: &str, label: &str, f: impl FnOnce(&mut StepPattern)) -> bool {
        match steps::edit_pattern(&mut self.project, clip, f) {
            Some(c) => self.edit_coalesced(key, label, vec![c]),
            None => false,
        }
    }

    /// Switch step `i` of row `row`.
    pub fn toggle_pattern_step(&mut self, clip: ClipId, row: usize, i: usize) -> bool {
        self.edit_steps(clip, "Toggle step", |p| {
            if let Some(r) = p.rows.get_mut(row) {
                r.edit_step(i, |s| s.on = !s.on);
            }
        })
    }

    /// Add a row below the last one, a key lower (or the first key).
    pub fn add_step_row(&mut self, clip: ClipId) -> bool {
        self.edit_steps(clip, "Add row", |p| {
            if p.rows.len() >= MAX_ROWS {
                return;
            }
            let key = p.rows.last().map_or(60, |r| r.key.saturating_sub(1));
            let mut row = StepRow::new(key);
            if let Some(last) = p.rows.last() {
                row.length = last.length;
                row.rate = last.rate;
            }
            p.rows.push(row);
        })
    }

    pub fn remove_step_row(&mut self, clip: ClipId, row: usize) -> bool {
        self.edit_steps(clip, "Delete row", |p| {
            if row < p.rows.len() {
                p.rows.remove(row);
            }
        })
    }

    /// Move row `row`'s key by `by` semitones.
    pub fn transpose_step_row(&mut self, clip: ClipId, row: usize, by: i32) -> bool {
        self.edit_steps(clip, "Change row key", |p| {
            if let Some(r) = p.rows.get_mut(row) {
                r.key = (i32::from(r.key) + by).clamp(0, 127) as u8;
            }
        })
    }

    /// Lengthen or shorten a row by `by` steps.
    pub fn resize_step_row(&mut self, clip: ClipId, row: usize, by: i32) -> bool {
        self.edit_steps(clip, "Change row length", |p| {
            if let Some(r) = p.rows.get_mut(row) {
                r.length = (i32::from(r.length) + by).clamp(1, i32::from(MAX_ROW_STEPS)) as u16;
            }
        })
    }

    /// Faster (`+1`) or slower (`-1`) steps for a row.
    pub fn step_row_rate(&mut self, clip: ClipId, row: usize, by: i32) -> bool {
        self.edit_steps(clip, "Change row rate", |p| {
            if let Some(r) = p.rows.get_mut(row) {
                r.rate = r.rate.step(by);
            }
        })
    }

    pub fn cycle_step_row_direction(&mut self, clip: ClipId, row: usize) -> bool {
        self.edit_steps(clip, "Change row direction", |p| {
            if let Some(r) = p.rows.get_mut(row) {
                r.direction = r.direction.next();
            }
        })
    }

    /// Row swing goes 0, 25, 50, 75 % and around.
    pub fn cycle_step_row_swing(&mut self, clip: ClipId, row: usize) -> bool {
        self.edit_steps(clip, "Change row swing", |p| {
            if let Some(r) = p.rows.get_mut(row) {
                r.swing = if r.swing >= 0.7 { 0.0 } else { (r.swing * 4.0).round() / 4.0 + 0.25 };
            }
        })
    }

    /// Turn the step clip into a plain notes clip.
    pub fn convert_to_notes(&mut self, clip: ClipId) -> bool {
        match steps::convert_to_notes(&self.project, clip) {
            Some(c) => {
                self.seal();
                let ok = self.edit("Convert to notes", vec![c]);
                self.seal();
                ok
            }
            None => false,
        }
    }

    /// Turn a notes clip into a step clip on sixteenth steps.
    pub fn convert_to_steps(&mut self, clip: ClipId) -> bool {
        match steps::convert_to_steps(&mut self.project, clip, StepRate::Sixteenth) {
            Some(cmds) => {
                self.seal();
                let ok = self.edit("Convert to steps", cmds);
                self.seal();
                ok
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_step_clip() -> (AppState, ClipId) {
        let mut p = Project::new("Test");
        let id = p.alloc_track_id();
        Command::AddTrack { track: Box::new(Track::new(id, TrackKind::Instrument, "Keys", Color::palette(0))), index: 0 }.apply(&mut p).unwrap();
        let mut s = AppState::new(p);
        s.selected_track = Some(id);
        let clip = s.insert_step_clip().expect("a step clip on the new track");
        (s, clip)
    }

    #[test]
    fn a_step_clip_edits_and_undoes_as_commands() {
        let (mut s, clip) = state_with_step_clip();
        assert_eq!(s.clip_pattern(clip).map(|p| p.rows.len()), Some(3));
        assert!(s.toggle_pattern_step(clip, 0, 4));
        assert_eq!(s.clip_notes(clip).map(|(_, n)| n.notes.len()), Some(1));
        assert!(s.resize_step_row(clip, 0, -4));
        assert_eq!(s.clip_pattern(clip).unwrap().rows[0].length, 12);
        assert!(s.undo());
        assert_eq!(s.clip_pattern(clip).unwrap().rows[0].length, 16);
        assert!(s.undo());
        assert_eq!(s.clip_notes(clip).map(|(_, n)| n.notes.len()), Some(0));
    }

    #[test]
    fn converting_keeps_the_notes_and_goes_back() {
        let (mut s, clip) = state_with_step_clip();
        s.toggle_pattern_step(clip, 1, 0);
        s.toggle_pattern_step(clip, 2, 8);
        assert!(s.convert_to_notes(clip));
        assert!(s.clip_pattern(clip).is_none());
        assert_eq!(s.clip_notes(clip).map(|(_, n)| n.notes.len()), Some(2));
        assert!(s.convert_to_steps(clip));
        assert_eq!(s.clip_pattern(clip).map(|p| p.rows.len()), Some(2));
    }

    #[test]
    fn rows_come_and_go() {
        let (mut s, clip) = state_with_step_clip();
        assert!(s.add_step_row(clip));
        assert_eq!(s.clip_pattern(clip).unwrap().rows.last().map(|r| r.key), Some(59));
        assert!(s.remove_step_row(clip, 0));
        assert_eq!(s.clip_pattern(clip).unwrap().rows.len(), 3);
        assert!(s.cycle_step_row_swing(clip, 0));
        assert_eq!(s.clip_pattern(clip).unwrap().rows[0].swing, 0.25);
    }
}
