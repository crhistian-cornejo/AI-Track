//! Track selection on the headers and group tracks, as in Live: click picks
//! a track, Cmd/Ctrl+click adds or removes one, Shift+click picks the range
//! of visible tracks from the last one picked; Cmd+G puts the picked tracks
//! in a new group and Cmd+Shift+G dissolves the picked groups. The rules of
//! the tree and the commands are in `aitrack_model::groups`.

use crate::state::AppState;
use aitrack_model::*;
use std::collections::BTreeSet;

impl AppState {
    /// The selected tracks in list order. Code that only writes
    /// `selected_track` selects that one track.
    pub fn selected_tracks(&self) -> Vec<TrackId> {
        let set: BTreeSet<TrackId> = match self.selected_track {
            Some(t) if self.ed.tracks.contains(&t) => self.ed.tracks.clone(),
            Some(t) => [t].into(),
            None => BTreeSet::new(),
        };
        self.project.tracks.iter().filter(|t| set.contains(&t.id)).map(|t| t.id).collect()
    }

    pub fn is_track_selected(&self, id: TrackId) -> bool {
        match self.selected_track {
            Some(t) if self.ed.tracks.contains(&t) => self.ed.tracks.contains(&id),
            Some(t) => t == id,
            None => false,
        }
    }

    /// A click on a track's header: `toggle` (Cmd/Ctrl) adds or removes it,
    /// `range` (Shift) picks every visible track from the last one picked.
    pub fn click_track(&mut self, id: TrackId, toggle: bool, range: bool) {
        if range && let Some(anchor) = self.selected_track {
            let lanes = self.lane_tracks();
            if let (Some(a), Some(b)) = (lanes.iter().position(|t| *t == anchor), lanes.iter().position(|t| *t == id)) {
                self.ed.tracks = lanes[a.min(b)..=a.max(b)].iter().copied().collect();
                // The anchor stays where it was, so the next Shift+click extends from it.
                return;
            }
        }
        if toggle {
            let mut set: BTreeSet<TrackId> = self.selected_tracks().into_iter().collect();
            if !set.remove(&id) {
                set.insert(id);
                self.selected_track = Some(id);
            } else if self.selected_track == Some(id) {
                self.selected_track = set.iter().next().copied().or(Some(id));
            }
            if set.is_empty() {
                set.insert(id);
            }
            self.ed.tracks = set;
            return;
        }
        self.ed.tracks = [id].into();
        self.selected_track = Some(id);
    }

    /// Cmd+G: the selected tracks go in a new group, which is selected.
    pub fn group_tracks(&mut self) -> Option<TrackId> {
        let ids = self.selected_tracks();
        let group = self.project.alloc_track_id();
        let cmds = groups::group_commands(&self.project, &ids, group)?;
        self.seal();
        let ok = self.edit("Group tracks", cmds);
        self.seal();
        ok.then(|| {
            self.selected_track = Some(group);
            self.ed.tracks = [group].into();
            group
        })
    }

    /// Cmd+Shift+G: dissolve each selected group (a selected track that is
    /// not a group dissolves the group it is in). Its tracks stay selected.
    pub fn ungroup_tracks(&mut self) -> bool {
        let mut targets = Vec::new();
        for id in self.selected_tracks() {
            let Some(t) = self.project.track(id) else { continue };
            let g = if t.kind == TrackKind::Group { Some(id) } else { t.parent.filter(|p| self.project.track(*p).is_some_and(|g| g.kind == TrackKind::Group)) };
            if let Some(g) = g.filter(|g| !targets.contains(g)) {
                targets.push(g);
            }
        }
        // Each group is dissolved in the state the ones before it left.
        let mut scratch = self.project.clone();
        let mut cmds = Vec::new();
        let mut members = BTreeSet::new();
        for g in &targets {
            members.extend(scratch.tracks.iter().filter(|t| t.parent == Some(*g)).map(|t| t.id));
            let Some(step) = groups::ungroup_commands(&scratch, *g) else { continue };
            for c in &step {
                if c.clone().apply(&mut scratch).is_err() {
                    return false;
                }
            }
            cmds.extend(step);
        }
        members.retain(|m| !targets.contains(m));
        if cmds.is_empty() {
            return false;
        }
        self.seal();
        let ok = self.edit("Ungroup tracks", cmds);
        self.seal();
        if ok {
            self.selected_track = self.project.tracks.iter().find(|t| members.contains(&t.id)).map(|t| t.id).or(self.project.content_tracks().next().map(|t| t.id));
            self.ed.tracks = members;
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (AppState, Vec<TrackId>) {
        let mut s = AppState::new(Project::new("t"));
        let ids: Vec<TrackId> = (0..3).filter_map(|_| s.add_track(TrackKind::Audio)).collect();
        (s, ids)
    }

    #[test]
    fn header_clicks_build_a_track_selection() {
        let (mut s, ids) = state();
        s.click_track(ids[0], false, false);
        s.click_track(ids[2], false, true);
        assert_eq!(s.selected_tracks(), ids);
        s.click_track(ids[1], true, false);
        assert_eq!(s.selected_tracks(), vec![ids[0], ids[2]]);
        // A plain write to selected_track still means one track.
        s.selected_track = Some(ids[1]);
        assert_eq!(s.selected_tracks(), vec![ids[1]]);
    }

    #[test]
    fn cmd_g_groups_the_selection_and_undo_restores_it() {
        let (mut s, ids) = state();
        let before = s.project.tracks.clone();
        s.click_track(ids[0], false, false);
        s.click_track(ids[2], true, false);
        let g = s.group_tracks().unwrap();
        let t = |id| s.project.track(id).unwrap().clone();
        assert_eq!((t(ids[0]).parent, t(ids[2]).parent, t(ids[1]).parent), (Some(g), Some(g), None));
        assert_eq!(t(ids[2]).output, Output::Track(g));
        assert_eq!(s.selected_tracks(), vec![g]);
        // Nest: the group and the remaining track in an outer group.
        s.click_track(ids[1], true, false);
        let outer = s.group_tracks().unwrap();
        assert_eq!(s.project.track(g).unwrap().output, Output::Track(outer));
        assert_eq!(groups::depth(&s.project, ids[0]), 2);
        // Ungrouping the inner group hands its tracks to the outer one.
        s.click_track(g, false, false);
        assert!(s.ungroup_tracks());
        assert_eq!(s.project.track(ids[0]).unwrap().parent, Some(outer));
        assert_eq!(s.selected_tracks(), vec![ids[0], ids[2]]);
        for _ in 0..3 {
            s.undo();
        }
        assert_eq!(s.project.tracks, before);
    }
}
