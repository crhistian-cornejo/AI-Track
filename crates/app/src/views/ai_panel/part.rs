//! One generated part and what can be done with it: offer it as a
//! proposal, listen to it (apply it as a normal history step with the loop
//! on it, so it plays through the track's real devices and mix, and Cmd+Z
//! takes it off), keep it, vary it, regenerate it, hold layers or bars.
//!
//! Everything here is plain data and `Project`/`History` calls so it can be
//! tested without a window; `state.rs` wires it to the chat card and to the
//! transport.

use aitrack_ai::generate::lock::{self, Locks};
use aitrack_ai::generate::spec::{MidiSpec, Role};
use aitrack_ai::generate::{GenContext, GenRequest, Generated};
use aitrack_model::{Command, History, Origin, Project, Tick};

use super::midi_gen::{self, Dest, Prepared};

/// Where the card stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Offered as a proposal; the project is untouched.
    Pending,
    /// Applied for listening; Keep or Undo.
    Trying,
    Accepted,
    Discarded,
    /// The user took the listened part off with Cmd+Z.
    Undone,
}

pub struct Session {
    pub req: GenRequest,
    pub ctx: GenContext,
    pub generated: Generated,
    pub locks: Locks,
    pub dest: Dest,
    pub start: Tick,
    /// "Rules" or the model's name.
    pub origin: String,
    pub proposal: Option<u64>,
    /// The history revision right after the part was applied for listening.
    pub trial: Option<u64>,
    pub status: Status,
    pub summary: Vec<String>,
    /// Whether the part deletes notes (it replaces a clip's).
    pub destructive: bool,
}

impl Session {
    pub fn new(prepared: Prepared, generated: Generated, origin: impl Into<String>) -> Self {
        Self { req: prepared.req, ctx: prepared.ctx, generated, locks: Locks::default(), dest: prepared.dest, start: prepared.start, origin: origin.into(), proposal: None, trial: None, status: Status::Pending, summary: Vec::new(), destructive: false }
    }

    /// The title of the card and the name of the history step.
    pub fn label(&self) -> String {
        self.generated.interpretation.clone()
    }

    /// The layers that can be held, in order, and whether each is.
    pub fn layers(&self) -> Vec<(Role, bool)> {
        let mut v: Vec<(Role, bool)> = Vec::new();
        for l in &self.generated.spec.layers {
            if !v.iter().any(|(r, _)| *r == l.role) {
                v.push((l.role, self.locks.roles.contains(&l.role)));
            }
        }
        v
    }

    pub fn seed(&self) -> u64 {
        self.generated.params.seed
    }

    fn drop_offer(&mut self, h: &mut History) {
        if let Some(id) = self.proposal.take() {
            h.discard(id);
        }
    }

    /// The commands for the current part, and the card lines.
    fn build(&mut self, p: &mut Project) -> Vec<Command> {
        let (cmds, summary) = midi_gen::commands(p, &self.generated, self.dest, self.start);
        self.destructive = cmds.iter().any(Command::is_destructive);
        self.summary = summary;
        cmds
    }

    /// Offer the part as a proposal (replacing the one pending).
    pub fn offer(&mut self, p: &mut Project, h: &mut History) -> Result<(), String> {
        self.drop_offer(h);
        let cmds = self.build(p);
        let id = h.propose(p, self.label(), Origin::Feature("midi-gen".into()), cmds).map_err(|e| e.to_string())?;
        self.proposal = Some(id);
        self.status = Status::Pending;
        Ok(())
    }

    /// The stretch of the timeline the part covers.
    fn region(&self, p: &Project) -> (Tick, Tick) {
        let len = match self.dest {
            Dest::Replace { clip, .. } => p.clips.get(&clip).map_or(self.generated.clip_ticks, |c| c.length),
            _ => self.generated.clip_ticks,
        };
        (self.start, self.start + len)
    }

    /// Apply the part as a normal step with the loop on it. Gives the loop.
    pub fn listen(&mut self, p: &mut Project, h: &mut History) -> Result<(Tick, Tick), String> {
        let region = self.region(p);
        if self.status == Status::Trying {
            return Ok(region);
        }
        self.drop_offer(h);
        let mut cmds = self.build(p);
        cmds.push(Command::SetLoop { region: Some(region) });
        h.apply(p, self.label(), Origin::Feature("midi-gen".into()), cmds).map_err(|e| e.to_string())?;
        self.trial = Some(h.revision());
        self.status = Status::Trying;
        Ok(region)
    }

    /// Keep what was applied for listening, or accept the pending proposal.
    pub fn accept(&mut self, p: &mut Project, h: &mut History) -> Result<(), String> {
        match self.status {
            Status::Pending => {
                let id = self.proposal.take().ok_or("There is no proposal to accept.")?;
                h.accept(p, id).map_err(|e| e.to_string())?;
                self.status = Status::Accepted;
                Ok(())
            }
            Status::Trying => {
                self.trial = None;
                self.status = Status::Accepted;
                Ok(())
            }
            _ => Err("This part is already closed.".into()),
        }
    }

    /// Take the listened part off, if nothing else was done since.
    pub fn undo_trial(&mut self, p: &mut Project, h: &mut History) -> Result<(), String> {
        if self.status != Status::Trying {
            return Ok(());
        }
        if Some(h.revision()) != self.trial {
            self.trial = None;
            self.status = Status::Accepted;
            return Err("You edited the project since, so the part stays; undo it with Cmd+Z when you want.".into());
        }
        h.undo(p).map_err(|e| e.to_string())?;
        self.trial = None;
        self.status = Status::Pending;
        Ok(())
    }

    pub fn discard(&mut self, p: &mut Project, h: &mut History) {
        match self.status {
            Status::Pending => {
                self.drop_offer(h);
                self.status = Status::Discarded;
            }
            Status::Trying => {
                if self.undo_trial(p, h).is_ok() {
                    self.status = Status::Discarded;
                }
            }
            Status::Undone => self.status = Status::Discarded,
            Status::Accepted | Status::Discarded => {}
        }
    }

    pub fn can_revise(&self) -> bool {
        matches!(self.status, Status::Pending | Status::Trying | Status::Undone)
    }

    /// Put `g` in place of the current part, in the same state (pending or listening).
    pub fn revise(&mut self, g: Generated, p: &mut Project, h: &mut History) -> Result<(), String> {
        match self.status {
            Status::Trying => {
                self.undo_trial(p, h)?;
                self.generated = g;
                self.listen(p, h).map(|_| ())
            }
            Status::Pending | Status::Undone => {
                self.drop_offer(h);
                self.generated = g;
                self.offer(p, h)
            }
            _ => Err("This part is already closed.".into()),
        }
    }

    /// The same spec with a new seed, around what is held.
    pub fn vary(&mut self, seed: u64, p: &mut Project, h: &mut History) -> Result<(), String> {
        let g = lock::vary(&self.generated, &self.req, &self.ctx, &self.locks, seed);
        self.revise(g, p, h)
    }

    /// A new spec (`spec` from a model, else a template) around what is held.
    pub fn regenerate(&mut self, seed: u64, spec: Option<MidiSpec>, p: &mut Project, h: &mut History) -> Result<(), String> {
        let g = lock::regenerate(&self.generated, &self.req, &self.ctx, &self.locks, seed, spec);
        self.revise(g, p, h)
    }

    /// Notice a Cmd+Z (or any edit) made while listening.
    pub fn sync(&mut self, h: &History) {
        let (Status::Trying, Some(t)) = (self.status, self.trial) else { return };
        if h.revision() == t {
            return;
        }
        self.status = if h.revision() == t + 1 && h.redo_label() == Some(self.label().as_str()) { Status::Undone } else { Status::Accepted };
        self.trial = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_ai::generate::spec::{Genre, Kind};
    use aitrack_ai::generate::generate;
    use aitrack_model::{Chain, Color, Device, DeviceKind, Key, Rack, RackKind, Scale, ScaleKind, Track, TrackId, TrackKind};

    fn project() -> (Project, TrackId) {
        let mut p = Project::new("t");
        p.scale = Some(Scale::new(Key(9), ScaleKind::Minor));
        let id = p.alloc_track_id();
        p.tracks.insert(0, Track::new(id, TrackKind::Instrument, "Bass", Color::palette(2)));
        (p, id)
    }

    fn kit() -> (Project, TrackId) {
        let (mut p, id) = project();
        let mut rack = Rack::new(RackKind::Drum);
        for (name, note) in [("Kick", 36u8), ("Snare", 38), ("Clap", 39), ("Closed Hat", 42), ("Open Hat", 46)] {
            let mut c = Chain::new(name, Color(0));
            c.note = Some(note);
            rack.chains.push(c);
        }
        let dev = p.alloc_device_id();
        let mut d = Device::stock(dev, "rack.drum", "Drum Rack");
        d.kind = DeviceKind::Rack(rack);
        p.track_mut(id).unwrap().devices.push(d);
        (p, id)
    }

    fn session(p: &Project, track: TrackId, kind: Kind, seed: u64) -> Session {
        let prep = midi_gen::prepare(p, Some(track), None, Some(kind), "2 bars").unwrap();
        let g = generate(&prep.req, &prep.ctx, seed);
        Session::new(prep, g, "Rules")
    }

    /// The projects are the same but for the id counter, which a build of
    /// commands moves and an undo does not give back.
    fn same(a: &Project, b: &Project) -> bool {
        let mut a = a.clone();
        a.ids = b.ids.clone();
        a == *b
    }

    fn note_keys(p: &Project) -> Vec<(u8, Tick)> {
        let mut v: Vec<_> = p.sources.values().flat_map(|s| match s {
            aitrack_model::Source::Notes(n) => n.notes.iter().map(|n| (n.key, n.start)).collect::<Vec<_>>(),
            _ => Vec::new(),
        }).collect();
        v.sort();
        v
    }

    #[test]
    fn offering_changes_nothing_and_accepting_is_one_undo_step() {
        let (mut p, track) = project();
        let mut h = History::default();
        let mut s = session(&p, track, Kind::Bass, 1);
        let before = p.clone();
        s.offer(&mut p, &mut h).unwrap();
        assert_eq!((s.status, h.proposals().len()), (Status::Pending, 1));
        assert!(same(&p, &before));
        s.accept(&mut p, &mut h).unwrap();
        assert_eq!(s.status, Status::Accepted);
        assert_eq!(p.clips.len(), 1);
        assert!(h.undo(&mut p).unwrap());
        assert_eq!(p.clips.len(), 0);
        assert!(!h.can_undo() || h.undo_label() != Some(s.label().as_str()));
    }

    #[test]
    fn listening_applies_the_part_with_the_loop_and_undo_puts_everything_back() {
        let (mut p, track) = project();
        let mut h = History::default();
        let mut s = session(&p, track, Kind::Bass, 1);
        s.offer(&mut p, &mut h).unwrap();
        let before = p.clone();
        let (a, b) = s.listen(&mut p, &mut h).unwrap();
        assert_eq!(s.status, Status::Trying);
        assert!(h.proposals().is_empty(), "the pending proposal became the applied step");
        assert_eq!((a, b), (0, 2 * 4 * 960));
        assert_eq!(p.loop_region, Some((a, b)), "the loop is on the part");
        assert_eq!(p.clips.len(), 1);
        assert!(h.is_dirty(), "listening marks the project as modified");
        s.undo_trial(&mut p, &mut h).unwrap();
        assert!(same(&p, &before), "one step back: clip, notes and loop");
        assert_eq!((s.status, h.proposals().len()), (Status::Pending, 0));
        s.offer(&mut p, &mut h).unwrap();
        assert_eq!(h.proposals().len(), 1, "it can be offered again");
    }

    #[test]
    fn undo_is_refused_when_the_user_edited_meanwhile() {
        let (mut p, track) = project();
        let mut h = History::default();
        let mut s = session(&p, track, Kind::Bass, 1);
        s.listen(&mut p, &mut h).unwrap();
        h.apply(&mut p, "Set loop", Origin::User, vec![Command::SetLoop { region: Some((0, 960)) }]).unwrap();
        let after_edit = p.clone();
        let err = s.undo_trial(&mut p, &mut h).unwrap_err();
        assert!(err.contains("edited"), "{err}");
        assert_eq!(p, after_edit, "nothing is touched");
        assert_eq!(s.status, Status::Accepted, "the part stays");
    }

    #[test]
    fn cmd_z_while_listening_is_noticed() {
        let (mut p, track) = project();
        let mut h = History::default();
        let mut s = session(&p, track, Kind::Bass, 1);
        s.listen(&mut p, &mut h).unwrap();
        s.sync(&h);
        assert_eq!(s.status, Status::Trying);
        h.undo(&mut p).unwrap();
        s.sync(&h);
        assert_eq!(s.status, Status::Undone);
        assert!(p.clips.is_empty());
        // A new take can still be offered from there.
        s.vary(77, &mut p, &mut h).unwrap();
        assert_eq!((s.status, h.proposals().len()), (Status::Pending, 1));
    }

    #[test]
    fn vary_while_listening_swaps_the_part_in_one_step() {
        let (mut p, track) = kit();
        let mut h = History::default();
        let mut s = session(&p, track, Kind::Drums, 1);
        s.listen(&mut p, &mut h).unwrap();
        let first = note_keys(&p);
        s.vary(900, &mut p, &mut h).unwrap();
        assert_eq!(s.status, Status::Trying);
        assert_eq!(p.clips.len(), 1, "still one clip");
        assert_eq!(s.seed(), 900);
        assert_ne!(note_keys(&p), first, "another take");
        assert!(h.undo(&mut p).unwrap());
        assert!(p.clips.is_empty(), "a single undo removes the part");
        assert_eq!(p.loop_region, Some((0, 16 * 960)), "and the loop it set");
    }

    #[test]
    fn held_layers_and_bars_keep_their_notes_through_vary_and_regenerate() {
        let (mut p, track) = kit();
        let mut h = History::default();
        let mut s = session(&p, track, Kind::Drums, 3);
        s.locks.toggle_role(Role::Kick);
        s.locks.toggle_role(Role::Clap);
        s.listen(&mut p, &mut h).unwrap();
        let kick_and_clap = |p: &Project| -> Vec<(u8, Tick)> { note_keys(p).into_iter().filter(|(k, _)| *k == 36 || *k == 39).collect() };
        let held = kick_and_clap(&p);
        assert!(!held.is_empty());
        for seed in [10, 11, 12] {
            s.vary(seed, &mut p, &mut h).unwrap();
            assert_eq!(kick_and_clap(&p), held, "vary, seed {seed}");
        }
        for seed in [20, 21] {
            s.regenerate(seed, None, &mut p, &mut h).unwrap();
            assert_eq!(kick_and_clap(&p), held, "regenerate, seed {seed}");
        }
        assert!(s.layers().iter().any(|(r, on)| *r == Role::Kick && *on));
        assert!(s.layers().iter().any(|(r, on)| *r == Role::Hat && !*on));
    }

    #[test]
    fn discard_takes_a_listened_part_off_and_drops_a_pending_one() {
        let (mut p, track) = project();
        let mut h = History::default();
        let mut s = session(&p, track, Kind::Chords, 2);
        let before = p.clone();
        s.offer(&mut p, &mut h).unwrap();
        s.discard(&mut p, &mut h);
        assert_eq!((s.status, h.proposals().len()), (Status::Discarded, 0));
        let mut s = session(&p, track, Kind::Chords, 2);
        s.listen(&mut p, &mut h).unwrap();
        s.discard(&mut p, &mut h);
        assert_eq!(s.status, Status::Discarded);
        assert!(same(&p, &before));
        assert!(!s.can_revise());
    }

    #[test]
    fn the_card_data_says_what_can_be_held() {
        let (p, track) = kit();
        let mut s = session(&p, track, Kind::Drums, 1);
        assert!(s.layers().len() >= 4);
        let (p2, t2) = project();
        let s2 = session(&p2, t2, Kind::Bass, 1);
        assert_eq!(s2.layers(), vec![(Role::Bass, false)]);
        assert!(s2.label().starts_with("Bass"));
        s.locks.toggle_bar(1);
        assert_eq!(s.locks.bars, vec![1]);
        let _ = (Genre::House, &s2.ctx);
    }
}
