//! Undo/redo and reviewable proposals.
//!
//! A [`Transaction`] is what the user sees as one undo step: a label, who
//! made it ([`Origin`]) and the commands to replay forward and backward.
//! AI agents never edit directly: they submit a [`Proposal`] that the UI can
//! preview on a clone and the user accepts (one undo step) or discards.

use crate::command::{Command, CommandError};
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
        let inverse = Command::Batch { commands: commands.clone() }.apply(project)?;
        let Command::Batch { commands: backward } = inverse else { unreachable!() };
        self.redo.clear();
        self.revision += 1;

        if let (Some(key), Some(last)) = (&coalesce, self.undo.last_mut())
            && last.coalesce.as_ref() == Some(key)
        {
            last.forward.extend(commands);
            let mut merged = backward;
            merged.append(&mut last.backward);
            last.backward = merged;
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
        let Some(mut t) = self.undo.pop() else { return Ok(false) };
        Command::Batch { commands: t.backward.clone() }.apply(project)?;
        t.coalesce = None;
        self.redo.push(t);
        self.revision += 1;
        Ok(true)
    }

    pub fn redo(&mut self, project: &mut Project) -> Result<bool, CommandError> {
        let Some(t) = self.redo.pop() else { return Ok(false) };
        Command::Batch { commands: t.forward.clone() }.apply(project)?;
        self.undo.push(t);
        self.revision += 1;
        Ok(true)
    }

    /// Recent steps, newest first (for a history panel).
    pub fn steps(&self) -> impl Iterator<Item = &Transaction> {
        self.undo.iter().rev()
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
    fn undo_redo() {
        let (mut p, mut h, id) = setup();
        h.apply(&mut p, "Vol", Origin::User, vec![Command::SetTrack { id, change: TrackProp::VolumeDb(-3.0) }]).unwrap();
        assert!(h.undo(&mut p).unwrap());
        assert_eq!(p.track(id).unwrap().volume_db, 0.0);
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
