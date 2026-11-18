//! One clip per moment on a track, as in Live.
//!
//! A clip that lands on others (dropped from the browser, pasted, moved,
//! duplicated, made longer) replaces what it covers on its track: clips
//! wholly under it go, clips it covers in part are cut at its edges and keep
//! the rest. The clips that land are the *winners* of the edit; [`landed`]
//! reads them off its commands and [`Plan::settle`] makes room for them.
//! [`AppState::overwriting`] does both for an edit that applies at once; a
//! drag collects its winners frame by frame and settles when it ends, so a
//! clip dragged across others only replaces what it is dropped on.

use super::clipboard::Plan;
use crate::state::AppState;
use aitrack_model::*;

/// The placements `cmds` put somewhere or make longer, added to `out` in
/// order (each once, at its last mention: a later one wins over an earlier
/// one that it lands on). `p` is the project before the commands apply.
pub(crate) fn landed(p: &Project, cmds: &[Command], out: &mut Vec<PlacementId>) {
    let win = |id: PlacementId, out: &mut Vec<PlacementId>| {
        out.retain(|x| *x != id);
        out.push(id);
    };
    for cmd in cmds {
        match cmd {
            Command::PlaceClip { placement, .. } => win(placement.id, out),
            Command::MovePlacement { id, .. } => win(*id, out),
            // Only a clip that grows can reach a neighbour: a new gain, name
            // or fade never trims the clips around it.
            Command::UpdateClip { id, clip } if p.clips.get(id).is_some_and(|old| clip.length > old.length) => {
                for pl in p.tracks.iter().flat_map(|t| t.arrangement.iter()).filter(|pl| pl.clip == *id) {
                    win(pl.id, out);
                }
            }
            Command::Batch { commands } => landed(p, commands, out),
            _ => {}
        }
    }
}

impl Plan {
    /// Make room for `winners`: what else they cover on their track and lane
    /// goes, cut at their edges. A winner gives way only to a later one.
    pub(crate) fn settle(&mut self, winners: &[PlacementId]) {
        for (i, id) in winners.iter().enumerate() {
            let found = self.p.tracks.iter().find_map(|t| t.arrangement.iter().find(|pl| pl.id == *id).map(|pl| (t.id, pl.clone())));
            let Some((track, pl)) = found else { continue };
            let Some(len) = self.p.clips.get(&pl.clip).map(|c| c.length) else { continue };
            self.clear_under(track, pl.start, pl.start + len, pl.lane, &winners[i..]);
        }
    }

    /// Empty `a..b` of `lane` on `track`, leaving the placements in `keep`.
    fn clear_under(&mut self, track: TrackId, a: Tick, b: Tick, lane: u16, keep: &[PlacementId]) {
        if b <= a {
            return;
        }
        let gives_way = |pl: &Placement| pl.lane == lane && !keep.contains(&pl.id);
        for edge in [a, b] {
            for pl in self.placements_in(track, lane, |x, y| x < edge && edge < y) {
                if gives_way(&pl)
                    && let Some(cmds) = ops::split_at(&mut self.p, track, pl.id, edge)
                {
                    self.run_all(cmds);
                }
            }
        }
        for pl in self.placements_in(track, lane, |x, y| x >= a && y <= b) {
            if gives_way(&pl) {
                self.remove(track, &pl);
            }
        }
    }
}

impl AppState {
    /// `commands` plus what it takes for the clips they land to replace what
    /// they cover, worked out on a scratch copy; `commands` as they came when
    /// they land nothing, cover nothing or would not apply.
    pub(crate) fn overwriting(&mut self, commands: Vec<Command>) -> Vec<Command> {
        let mut winners = Vec::new();
        landed(&self.project, &commands, &mut winners);
        if winners.is_empty() {
            return commands;
        }
        let mut plan = Plan::new(self);
        plan.run_all(commands.iter().cloned());
        let asked = plan.cmds.len();
        plan.settle(&winners);
        if plan.failed() || plan.cmds.len() == asked {
            return commands;
        }
        plan.sweep();
        // The ids the plan handed out are taken.
        self.project.ids = plan.p.ids.clone();
        plan.cmds
    }

    /// Remember the clips a frame of a drag lands, before it applies.
    pub(crate) fn note_landed(&mut self, commands: &[Command]) {
        let mut winners = std::mem::take(&mut self.ed.landed);
        landed(&self.project, commands, &mut winners);
        self.ed.landed = winners;
    }

    /// The end of a gesture: what it takes for the clips it moved or made
    /// longer to replace what they now cover; empty when they cover nothing.
    pub(crate) fn settle_gesture(&mut self) -> Vec<Command> {
        let winners = std::mem::take(&mut self.ed.landed);
        if winners.is_empty() {
            return Vec::new();
        }
        let mut plan = Plan::new(self);
        plan.settle(&winners);
        if plan.failed() || plan.cmds.is_empty() {
            return Vec::new();
        }
        plan.sweep();
        self.project.ids = plan.p.ids.clone();
        plan.cmds
    }
}
