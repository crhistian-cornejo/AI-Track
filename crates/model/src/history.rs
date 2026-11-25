//! Undo/redo and reviewable proposals.
//!
//! A [`Transaction`] is what the user sees as one undo step: a label, who
//! made it ([`Origin`]) and the commands to replay forward and backward.
//! AI agents never edit directly: they submit a [`Proposal`] that the UI can
//! preview on a clone and the user accepts (one undo step) or discards.

use crate::command::{ChainProp, Command, CommandError, TrackProp};
use crate::project::Project;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "name")]
pub enum Origin {
    User,
    /// An AI agent (Claude Code, Codex, Droid…) connected over MCP.
    Agent(String),
    /// A built-in AI feature (audio-to-MIDI, stem split…).
    Feature(String),
    Script(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub label: String,
    pub origin: Origin,
    forward: Vec<Command>,
    backward: Vec<Command>,
    /// Consecutive transactions with the same key merge (knob drags).
    coalesce: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Proposal {
    pub id: u64,
    pub label: String,
    pub origin: Origin,
    pub commands: Vec<Command>,
}

impl Proposal {
    /// The project as it would look if accepted.
    pub fn preview(&self, project: &Project) -> Result<Project, CommandError> {
        let mut p = project.clone();
        Command::Batch { commands: self.commands.clone() }.apply(&mut p)?;
        Ok(p)
    }

    pub fn is_destructive(&self) -> bool {
        self.commands.iter().any(Command::is_destructive)
    }
}

/// What an absolute setter writes to. Two commands with the same slot are the
/// same write: the later one wins whatever the earlier one did.
#[derive(PartialEq)]
enum Slot {
    Notes(crate::SourceId, Vec<crate::NoteId>),
    Track(crate::TrackId, std::mem::Discriminant<TrackProp>),
    Param(crate::DeviceId, u32),
    Macro(crate::DeviceId, usize),
    Chain(crate::DeviceId, usize, bool, std::mem::Discriminant<ChainProp>, usize),
    Mapping(crate::DeviceId, usize, usize),
    Selector(crate::DeviceId),
    Steps(crate::SourceId),
    Tempo,
    Loop,
    Clip(crate::ClipId),
    Warp(crate::SourceId),
    Groove(crate::GrooveId),
    GrooveSettings(crate::GrooveId),
    GrooveAmount,
    Locator(crate::LocatorId),
    Automation(crate::TrackId, crate::AutomationTarget),
}

/// The slot of a command that is a plain "set this to X" whose effect does not
/// depend on the state before it; `None` for everything else (adds, removes,
/// moves relative to the current place, batches…).
fn slot(c: &Command) -> Option<Slot> {
    Some(match c {
        Command::SetStepPattern { source, .. } => Slot::Steps(*source),
        Command::UpdateNotes { source, notes } => Slot::Notes(*source, notes.iter().map(|n| n.id).collect()),
        Command::SetTrack { id, change } => Slot::Track(*id, std::mem::discriminant(change)),
        Command::SetParam { device, param, .. } => Slot::Param(*device, *param),
        Command::SetMacro { rack, index, .. } => Slot::Macro(*rack, *index),
        Command::SetChainProp { rack, chain, prop } => Slot::Chain(*rack, *chain, false, std::mem::discriminant(prop), send_index(prop)),
        Command::SetReturnProp { rack, index, prop } => Slot::Chain(*rack, *index, true, std::mem::discriminant(prop), send_index(prop)),
        Command::SetRackSelector { rack, .. } => Slot::Selector(*rack),
        Command::SetMappingRange { rack, macro_index, index, .. } => Slot::Mapping(*rack, *macro_index, *index),
        Command::SetTempo { .. } => Slot::Tempo,
        Command::SetLoop { .. } => Slot::Loop,
        Command::UpdateClip { id, .. } => Slot::Clip(*id),
        Command::SetWarp { source, .. } => Slot::Warp(*source),
        Command::UpdateGroove { id, .. } => Slot::Groove(*id),
        Command::SetGrooveSettings { id, .. } => Slot::GrooveSettings(*id),
        Command::SetGrooveAmount { .. } => Slot::GrooveAmount,
        Command::MoveLocator { id, .. } => Slot::Locator(*id),
        Command::SetAutomation { track, target, .. } => Slot::Automation(*track, *target),
        _ => return None,
    })
}

/// Sends are one slot each (`Send(0, ..)` and `Send(1, ..)` are not the same write).
fn send_index(prop: &ChainProp) -> usize {
    match prop {
        ChainProp::Send(i, _) => *i,
        _ => 0,
    }
}

/// Whether `new` writes exactly the slots `old` wrote, in the same order, so
/// `new` can take the place of `old` in a coalesced step.
fn replaces(old: &[Command], new: &[Command]) -> bool {
    old.len() == new.len() && old.iter().zip(new).all(|(a, b)| matches!((slot(a), slot(b)), (Some(x), Some(y)) if x == y))
}

#[derive(Debug)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    limit: usize,
    /// Bumps on every change; the engine and autosave watch it.
    revision: u64,
    saved_revision: u64,
    pending: Vec<Proposal>,
    next_proposal: u64,
}

impl Default for History {
    fn default() -> Self {
        Self::new(500)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: Vec::new(), redo: Vec::new(), limit, revision: 0, saved_revision: 0, pending: Vec::new(), next_proposal: 1 }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn mark_saved(&mut self) {
        self.saved_revision = self.revision;
    }

    /// Apply commands as one undo step.
    pub fn apply(&mut self, project: &mut Project, label: impl Into<String>, origin: Origin, commands: Vec<Command>) -> Result<(), CommandError> {
        self.apply_inner(project, label.into(), origin, commands, None)
    }

    /// Like [`apply`](Self::apply) but merges with the previous step when it
    /// has the same `key` (a continuous drag becomes one undo step).
    pub fn apply_coalesced(&mut self, project: &mut Project, key: impl Into<String>, label: impl Into<String>, commands: Vec<Command>) -> Result<(), CommandError> {
        self.apply_inner(project, label.into(), Origin::User, commands, Some(key.into()))
    }

    /// Apply `commands` as part of the last undo step: what a finished
    /// gesture still has to settle (the clips it dropped a clip on, say), so
    /// one undo takes both back. A step of its own when there is none.
    pub fn amend(&mut self, project: &mut Project, commands: Vec<Command>) -> Result<(), CommandError> {
        if commands.is_empty() {
            return Ok(());
        }
        if self.undo.is_empty() {
            return self.apply(project, "Edit", Origin::User, commands);
        }
        if let Some(track) = commands.iter().find_map(|c| c.frozen_target(project)) {
            return Err(CommandError::Frozen(track));
        }
        let inverse = Command::Batch { commands: commands.clone() }.apply(project)?;
        let Command::Batch { commands: mut backward } = inverse else { unreachable!() };
        self.redo.clear();
        self.revision += 1;
        if let Some(last) = self.undo.last_mut() {
            last.forward.extend(commands);
            backward.append(&mut last.backward);
            last.backward = backward;
        }
        Ok(())
    }

    /// Break coalescing so the next drag starts a fresh undo step.
    pub fn seal(&mut self) {
        if let Some(t) = self.undo.last_mut() {
            t.coalesce = None;
        }
    }

    fn apply_inner(&mut self, project: &mut Project, label: String, origin: Origin, commands: Vec<Command>, coalesce: Option<String>) -> Result<(), CommandError> {
        if commands.is_empty() {
            return Ok(());
        }
        if let Some(track) = commands.iter().find_map(|c| c.frozen_target(project)) {
            return Err(CommandError::Frozen(track));
        }
        let inverse = Command::Batch { commands: commands.clone() }.apply(project)?;
        let Command::Batch { commands: backward } = inverse else { unreachable!() };
        self.redo.clear();
        self.revision += 1;

        if let (Some(key), Some(last)) = (&coalesce, self.undo.last_mut())
            && last.coalesce.as_ref() == Some(key)
        {
            if replaces(&last.forward, &commands) {
                // The same absolute setters again (a drag frame): the first
                // frame's `backward` already restores the start, and the new
                // frame's `forward` already reaches the end. Keep one copy.
                last.forward = commands;
            } else {
                last.forward.extend(commands);
                let mut merged = backward;
                merged.append(&mut last.backward);
                last.backward = merged;
            }
            return Ok(());
        }
        self.undo.push(Transaction { label, origin, forward: commands, backward, coalesce });
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|t| t.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|t| t.label.as_str())
    }

    pub fn undo(&mut self, project: &mut Project) -> Result<bool, CommandError> {
        let Some(t) = self.undo.last_mut() else { return Ok(false) };
        // A batch is atomic: when it fails the project is untouched, and so is the step.
        Command::Batch { commands: t.backward.clone() }.apply(project)?;
        t.coalesce = None;
        let t = self.undo.pop().expect("the step just undone");
        self.redo.push(t);
        self.revision += 1;
        Ok(true)
    }

    pub fn redo(&mut self, project: &mut Project) -> Result<bool, CommandError> {
        let Some(t) = self.redo.last() else { return Ok(false) };
        Command::Batch { commands: t.forward.clone() }.apply(project)?;
        let t = self.redo.pop().expect("the step just redone");
        self.undo.push(t);
        self.revision += 1;
        Ok(true)
    }

    /// Recent steps, newest first (for a history panel).
    pub fn steps(&self) -> impl Iterator<Item = &Transaction> {
        self.undo.iter().rev()
    }

    /// Undone steps waiting to be redone, next-to-redo first.
    pub fn redo_steps(&self) -> impl Iterator<Item = &Transaction> {
        self.redo.iter().rev()
    }

    // ── Proposals ──

    /// Stage commands for review. Validates them against a clone first.
    pub fn propose(&mut self, project: &Project, label: impl Into<String>, origin: Origin, commands: Vec<Command>) -> Result<u64, CommandError> {
        let proposal = Proposal { id: self.next_proposal, label: label.into(), origin, commands };
        proposal.preview(project)?;
        self.next_proposal += 1;
        let id = proposal.id;
        self.pending.push(proposal);
        Ok(id)
    }

    pub fn proposals(&self) -> &[Proposal] {
        &self.pending
    }

    pub fn accept(&mut self, project: &mut Project, id: u64) -> Result<bool, CommandError> {
        let Some(i) = self.pending.iter().position(|p| p.id == id) else { return Ok(false) };
        let p = self.pending.remove(i);
        self.apply(project, p.label, p.origin, p.commands)?;
        Ok(true)
    }

    pub fn discard(&mut self, id: u64) -> bool {
        let before = self.pending.len();
        self.pending.retain(|p| p.id != id);
        before != self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::TrackProp;
    use crate::project::*;

    fn setup() -> (Project, History, crate::TrackId) {
        let mut p = Project::new("t");
        let id = p.alloc_track_id();
        let mut h = History::default();
        h.apply(&mut p, "Add", Origin::User, vec![Command::AddTrack { track: Box::new(Track::new(id, TrackKind::Audio, "A", Color::palette(0))), index: 1 }]).unwrap();
        (p, h, id)
    }

    #[test]
    fn amend_joins_the_last_step() {
        let (mut p, mut h, id) = setup();
        h.apply_coalesced(&mut p, "vol", "Vol", vec![Command::SetTrack { id, change: TrackProp::VolumeDb(-3.0) }]).unwrap();
        let steps = h.steps().count();
        h.amend(&mut p, vec![Command::SetTrack { id, change: TrackProp::Pan(0.5) }]).unwrap();
        assert_eq!(h.steps().count(), steps, "no step of its own");
        assert_eq!((p.track(id).unwrap().volume_db, p.track(id).unwrap().pan), (-3.0, 0.5));
        assert!(h.undo(&mut p).unwrap());
        assert_eq!((p.track(id).unwrap().volume_db, p.track(id).unwrap().pan), (0.0, 0.0), "one undo takes both back");
        assert!(h.redo(&mut p).unwrap());
        assert_eq!((p.track(id).unwrap().volume_db, p.track(id).unwrap().pan), (-3.0, 0.5));
        // Nothing to amend: nothing changes, not even the revision.
        let rev = h.revision();
        h.amend(&mut p, Vec::new()).unwrap();
        assert_eq!(h.revision(), rev);
    }

    #[test]
    fn undo_redo() {
        let (mut p, mut h, id) = setup();
        h.apply(&mut p, "Vol", Origin::User, vec![Command::SetTrack { id, change: TrackProp::VolumeDb(-3.0) }]).unwrap();
        assert!(h.undo(&mut p).unwrap());
        assert_eq!(p.track(id).unwrap().volume_db, 0.0);
        assert!(h.redo(&mut p).unwrap());
        assert_eq!(p.track(id).unwrap().volume_db, -3.0);
    }

    #[test]
    fn a_failed_undo_or_redo_keeps_the_step() {
        let (mut p, mut h, id) = setup();
        h.apply(&mut p, "Vol", Origin::User, vec![Command::SetTrack { id, change: TrackProp::VolumeDb(-3.0) }]).unwrap();
        // The track goes away behind the history's back: the step cannot apply now.
        let mut gone = p.clone();
        gone.tracks.retain(|t| t.id != id);
        assert!(h.undo(&mut gone).is_err());
        assert_eq!(h.undo_label(), Some("Vol"), "the step is still there to undo");
        assert!(h.undo(&mut p).unwrap());
        assert_eq!(p.track(id).unwrap().volume_db, 0.0);
        assert!(h.redo(&mut gone).is_err());
        assert_eq!(h.redo_label(), Some("Vol"), "the step is still there to redo");
        assert!(h.redo(&mut p).unwrap());
        assert_eq!(p.track(id).unwrap().volume_db, -3.0);
    }

    #[test]
    fn coalesced_drag_is_one_step() {
        let (mut p, mut h, id) = setup();
        for v in [-1.0, -2.0, -3.0] {
            h.apply_coalesced(&mut p, "fader", "Volume", vec![Command::SetTrack { id, change: TrackProp::VolumeDb(v) }]).unwrap();
        }
        h.undo(&mut p).unwrap();
        assert_eq!(p.track(id).unwrap().volume_db, 0.0);
        assert_eq!(h.undo_label(), Some("Add"));
    }

    /// A drag of many notes over 120 frames keeps one `forward` and one
    /// `backward` (the first frame's inverse), not 120 copies of the notes.
    #[test]
    fn coalesced_note_drag_keeps_one_copy_each_way() {
        let mut p = Project::new("t");
        let source = p.alloc_source_id();
        let notes: Vec<Note> = (0..500i64).map(|i| Note::new(p.alloc_note_id(), 60, i * 10, 5, 0.8)).collect();
        let mut h = History::default();
        h.apply(&mut p, "Add", Origin::User, vec![Command::AddSource { id: source, source: Box::new(Source::Notes(NoteSource { notes: notes.clone(), ..Default::default() })) }]).unwrap();
        for frame in 1..=120i64 {
            let moved: Vec<Note> = notes.iter().map(|n| Note { start: n.start + frame * 3, ..n.clone() }).collect();
            h.apply_coalesced(&mut p, "notes-move", "Move notes", vec![Command::UpdateNotes { source, notes: moved }]).unwrap();
        }
        let t = h.undo.last().unwrap();
        assert_eq!(t.forward.len(), 1, "forward is the last frame only");
        assert_eq!(t.backward.len(), 1, "backward is the first frame's inverse only");
        assert_eq!(h.steps().count(), 2);
        let start_of = |p: &Project| match p.sources.get(&source) {
            Some(Source::Notes(n)) => n.notes[0].start,
            _ => unreachable!(),
        };
        assert_eq!(start_of(&p), 360);
        h.undo(&mut p).unwrap();
        assert_eq!(start_of(&p), 0, "undo goes back to before the drag");
        h.redo(&mut p).unwrap();
        assert_eq!(start_of(&p), 360, "redo goes to where the drag ended");
    }

    /// Frames that touch different things cannot replace each other: they merge.
    #[test]
    fn coalescing_frames_with_different_targets_still_merges() {
        let (mut p, mut h, id) = setup();
        h.apply_coalesced(&mut p, "k", "Edit", vec![Command::SetTrack { id, change: TrackProp::VolumeDb(-1.0) }]).unwrap();
        h.apply_coalesced(&mut p, "k", "Edit", vec![Command::SetTrack { id, change: TrackProp::Pan(0.5) }]).unwrap();
        h.apply_coalesced(&mut p, "k", "Edit", vec![Command::SetTrack { id, change: TrackProp::Pan(0.7) }]).unwrap();
        h.undo(&mut p).unwrap();
        assert_eq!(p.track(id).unwrap().volume_db, 0.0);
        assert_eq!(p.track(id).unwrap().pan, 0.0);
        h.redo(&mut p).unwrap();
        assert_eq!(p.track(id).unwrap().volume_db, -1.0);
        assert_eq!(p.track(id).unwrap().pan, 0.7);
    }

    #[test]
    fn proposals_apply_as_one_step() {
        let (mut p, mut h, id) = setup();
        let pid = h
            .propose(&p, "Agent mix", Origin::Agent("claude".into()), vec![
                Command::SetTrack { id, change: TrackProp::VolumeDb(-6.0) },
                Command::SetTrack { id, change: TrackProp::Pan(0.3) },
            ])
            .unwrap();
        assert_eq!(p.track(id).unwrap().volume_db, 0.0, "proposal does not touch the project");
        h.accept(&mut p, pid).unwrap();
        assert_eq!(p.track(id).unwrap().pan, 0.3);
        h.undo(&mut p).unwrap();
        assert_eq!(p.track(id).unwrap().pan, 0.0);
        assert_eq!(p.track(id).unwrap().volume_db, 0.0);
    }

    #[test]
    fn invalid_proposal_is_rejected() {
        let (p, mut h, _) = setup();
        assert!(h.propose(&p, "bad", Origin::Agent("x".into()), vec![Command::RemoveTrack { id: crate::TrackId(777) }]).is_err());
        assert!(h.proposals().is_empty());
    }
}
