//! Capture Automation and Capture MIDI (Live 13): while the transport rolls
//! without recording, the last few minutes of knob moves and of notes played
//! on armed tracks are kept in a rolling buffer, each with the timeline
//! position it happened at. "Capture" writes them where they happened: the
//! moves as automation lanes, the notes as a MIDI clip, in one undo step.
//!
//! Knob moves come from the commands the interface applies (a fader, a
//! device knob, a plugin's own window); notes come from the engine's capture
//! queue, which the audio thread feeds without allocating (see
//! `aitrack_engine::record`). This module is the buffer and the pure parts;
//! the edits themselves are in [`crate::state::AppState`].

use crate::bridge::RecNote;
use aitrack_model::{AutomationPoint, AutomationTarget, Command, Project, Tick, TrackId, TrackProp};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// How far back the buffer reaches.
pub const WINDOW: Duration = Duration::from_secs(5 * 60);
/// Bounds so a long session never grows the buffer without limit.
const MAX_MOVES: usize = 50_000;
const MAX_NOTES: usize = 20_000;
/// Two moves of one control further apart than this are separate gestures.
const GESTURE_GAP: Duration = Duration::from_millis(400);
/// What a captured gesture may differ from the written line by (of the range).
const TOLERANCE: f32 = 0.004;
/// A note held while the transport stopped has no end: it is cut here.
const MAX_NOTE: Tick = 4 * 4 * aitrack_model::PPQ;

/// One move of a control, with where the playhead was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamMove {
    pub at: Instant,
    pub tick: Tick,
    pub track: TrackId,
    pub target: AutomationTarget,
    /// Normalised, as an automation lane holds it.
    pub value: f32,
}

/// What the knobs and the keyboard did lately.
#[derive(Default)]
pub struct Capture {
    moves: VecDeque<ParamMove>,
    notes: VecDeque<(Instant, RecNote)>,
    /// The playhead while the transport rolls without recording; `None` otherwise.
    clock: Option<Tick>,
}

impl Capture {
    /// Where moves land: the playhead while playing and not recording, else nowhere.
    pub fn set_clock(&mut self, playhead: Option<Tick>) {
        self.clock = playhead;
    }

    pub fn clear_moves(&mut self) {
        self.moves.clear();
    }

    pub fn clear_notes(&mut self) {
        self.notes.clear();
    }

    /// Note that a control moved to `value` now. Ignored unless the transport
    /// rolls (without recording); two moves at one tick keep the last.
    pub fn note_move(&mut self, now: Instant, track: TrackId, target: AutomationTarget, value: f32) {
        let Some(tick) = self.clock else { return };
        let value = value.clamp(0., 1.);
        if let Some(last) = self.moves.iter_mut().rev().find(|m| m.track == track && m.target == target)
            && last.tick == tick
        {
            last.value = value;
            last.at = now;
            return;
        }
        self.moves.push_back(ParamMove { at: now, tick, track, target, value });
        self.prune(now);
    }

    /// Notes the engine captured (released since the last call).
    pub fn push_notes(&mut self, now: Instant, notes: impl IntoIterator<Item = RecNote>) {
        for n in notes {
            self.notes.push_back((now, RecNote { length_ticks: n.length_ticks.min(MAX_NOTE), ..n }));
        }
        self.prune(now);
    }

    /// Forget what is older than [`WINDOW`] (and what overflows the bounds).
    pub fn prune(&mut self, now: Instant) {
        while self.moves.front().is_some_and(|m| now.saturating_duration_since(m.at) > WINDOW) || self.moves.len() > MAX_MOVES {
            self.moves.pop_front();
        }
        while self.notes.front().is_some_and(|n| now.saturating_duration_since(n.0) > WINDOW) || self.notes.len() > MAX_NOTES {
            self.notes.pop_front();
        }
    }

    /// The captured notes, oldest first.
    pub fn notes(&self) -> Vec<RecNote> {
        self.notes.iter().map(|n| n.1).collect()
    }

    /// The moves of each control as gestures: runs of breakpoints, each
    /// already thinned to what the eye can tell apart. Controls in the order
    /// they were first moved.
    pub fn gestures(&self) -> Vec<(TrackId, AutomationTarget, Vec<Vec<AutomationPoint>>)> {
        let mut lanes: Vec<(TrackId, AutomationTarget, Vec<Vec<AutomationPoint>>)> = Vec::new();
        let mut index: HashMap<(TrackId, AutomationTarget), usize> = HashMap::new();
        let mut last: HashMap<(TrackId, AutomationTarget), ParamMove> = HashMap::new();
        for m in &self.moves {
            let key = (m.track, m.target);
            let i = *index.entry(key).or_insert_with(|| {
                lanes.push((m.track, m.target, Vec::new()));
                lanes.len() - 1
            });
            let runs = &mut lanes[i].2;
            let continues = last.get(&key).is_some_and(|l| m.at.saturating_duration_since(l.at) <= GESTURE_GAP && m.tick >= l.tick);
            if !continues || runs.is_empty() {
                runs.push(Vec::new());
            }
            if let Some(run) = runs.last_mut() {
                run.push(AutomationPoint { tick: m.tick, value: m.value, curve: 0. });
            }
            last.insert(key, *m);
        }
        for (_, _, runs) in lanes.iter_mut() {
            for run in runs.iter_mut() {
                *run = aitrack_model::automation::simplify(run, TOLERANCE);
            }
        }
        lanes
    }
}

/// The control moves a gesture's commands make, as `(track, lane, value)`:
/// a device or rack parameter, a macro, a track's volume or pan.
pub fn moves_of(project: &Project, commands: &[Command]) -> Vec<(TrackId, AutomationTarget, f32)> {
    let mut out = Vec::new();
    for c in commands {
        match c {
            Command::SetParam { device, param, value: Some(v) } => {
                if let Some(track) = project.device_track(*device) {
                    out.push((track, AutomationTarget::DeviceParam { device: *device, param: *param }, *v));
                }
            }
            Command::SetMacro { rack, index, value } => {
                if let Some(track) = project.device_track(*rack) {
                    out.push((track, AutomationTarget::Macro { device: *rack, index: *index as u8 }, *value));
                }
            }
            Command::SetTrack { id, change: TrackProp::VolumeDb(db) } => {
                out.push((*id, AutomationTarget::Volume, aitrack_engine::automation::volume_normalized_from_db(*db)));
            }
            Command::SetTrack { id, change: TrackProp::Pan(p) } => {
                out.push((*id, AutomationTarget::Pan, (p.clamp(-1., 1.) + 1.) * 0.5));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: TrackId = TrackId(1);

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn note(key: u8, start: Tick) -> RecNote {
        RecNote { track: T, key, velocity: 0.8, start_tick: start, length_ticks: 240, pass: 0 }
    }

    #[test]
    fn nothing_is_kept_while_the_transport_is_not_rolling() {
        let (mut c, now) = (Capture::default(), Instant::now());
        c.note_move(now, T, AutomationTarget::Volume, 0.5);
        assert!(c.moves.is_empty());
        c.set_clock(Some(960));
        c.note_move(now, T, AutomationTarget::Volume, 0.5);
        assert!(!c.moves.is_empty());
        c.set_clock(None);
        c.note_move(at(now, 10), T, AutomationTarget::Volume, 0.7);
        assert_eq!(c.gestures().iter().map(|l| l.2.len()).sum::<usize>(), 1, "the move off-transport was ignored");
    }

    #[test]
    fn old_moves_and_notes_fall_out_of_the_window() {
        let (mut c, now) = (Capture::default(), Instant::now());
        c.set_clock(Some(0));
        c.note_move(now, T, AutomationTarget::Pan, 0.5);
        c.push_notes(now, [note(60, 0)]);
        let later = now + WINDOW + Duration::from_secs(1);
        c.push_notes(later, [note(62, 960)]);
        c.set_clock(Some(480));
        c.note_move(later, T, AutomationTarget::Pan, 0.6);
        assert_eq!(c.notes().len(), 1);
        assert_eq!(c.notes()[0].key, 62);
        let g = c.gestures();
        assert_eq!(g[0].2, vec![vec![AutomationPoint { tick: 480, value: 0.6, curve: 0. }]]);
    }

    #[test]
    fn a_pause_splits_gestures_and_dense_moves_are_thinned() {
        let (mut c, now) = (Capture::default(), Instant::now());
        // A sweep of 200 moves on a straight line, 5 ms apart, then another after a pause.
        for i in 0..200 {
            c.set_clock(Some(i * 4));
            c.note_move(at(now, i as u64 * 5), T, AutomationTarget::Volume, i as f32 / 200.);
        }
        c.set_clock(Some(5000));
        c.note_move(at(now, 5000), T, AutomationTarget::Volume, 0.2);
        let g = c.gestures();
        let runs = &g[0].2;
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len(), 2, "a straight sweep is two breakpoints: {:?}", runs[0]);
        assert_eq!(runs[0][0].tick, 0);
        assert_eq!(runs[0][1].tick, 796);
    }

    #[test]
    fn long_held_notes_are_cut_and_the_buffer_is_bounded() {
        let (mut c, now) = (Capture::default(), Instant::now());
        c.push_notes(now, [RecNote { length_ticks: 10_000_000, ..note(60, 0) }]);
        assert_eq!(c.notes()[0].length_ticks, MAX_NOTE);
        c.set_clock(Some(0));
        for i in 0..(MAX_MOVES + 10) {
            c.note_move(now, TrackId(i as u64 % 7 + 1), AutomationTarget::Volume, 0.5);
            c.set_clock(Some(i as Tick + 1));
        }
        assert!(c.moves.len() <= MAX_MOVES);
    }

    #[test]
    fn commands_map_to_lanes_with_normalised_values() {
        let p = Project::new("t");
        let track = TrackId(5);
        let moves = moves_of(
            &p,
            &[
                Command::SetTrack { id: track, change: TrackProp::Pan(-1.0) },
                Command::SetTrack { id: track, change: TrackProp::VolumeDb(-160.0) },
                Command::SetParam { device: aitrack_model::DeviceId(9), param: 0, value: Some(0.3) },
            ],
        );
        assert_eq!(moves.len(), 2, "a parameter of no device is skipped");
        assert_eq!(moves[0], (track, AutomationTarget::Pan, 0.0));
        assert_eq!(moves[1].1, AutomationTarget::Volume);
        assert!(moves[1].2 < 0.01);
    }
}
