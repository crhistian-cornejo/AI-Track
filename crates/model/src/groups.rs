//! Group tracks as pure command builders: grouping a selection, dissolving
//! a group, and the tree questions the views ask (how deep a track sits,
//! whether a folded group hides it, which tracks a group holds).
//!
//! The track list is a flattened tree: a group is followed by its members
//! (and theirs), each pointing back at it with `parent`. The members of a
//! group send their audio to it (`Output::Track(group)`), and the group goes
//! on to its own parent group or to Main, as in Ableton Live.

use crate::command::{Command, TrackProp};
use crate::ids::TrackId;
use crate::project::*;

/// How many groups `id` sits inside (0 for a top-level track).
pub fn depth(p: &Project, id: TrackId) -> usize {
    ancestors(p, id).count()
}

/// The groups (and folders) around `id`, innermost first.
pub fn ancestors(p: &Project, id: TrackId) -> impl Iterator<Item = TrackId> + '_ {
    let mut at = p.track(id).and_then(|t| t.parent);
    // Bounded by the track count, so a corrupt cycle cannot hang the UI.
    let mut left = p.tracks.len();
    std::iter::from_fn(move || {
        let id = at.filter(|_| left > 0)?;
        left -= 1;
        at = p.track(id).and_then(|t| t.parent);
        Some(id)
    })
}

/// Whether a folded group around `id` hides it.
pub fn hidden(p: &Project, id: TrackId) -> bool {
    ancestors(p, id).any(|g| p.track(g).is_some_and(|g| g.collapsed))
}

/// Whether `id` is `ancestor` or sits somewhere inside it.
pub fn within(p: &Project, id: TrackId, ancestor: TrackId) -> bool {
    id == ancestor || ancestors(p, id).any(|a| a == ancestor)
}

/// Every track inside `group`, at any depth, in list order.
pub fn descendants(p: &Project, group: TrackId) -> Vec<TrackId> {
    p.tracks.iter().filter(|t| t.id != group && within(p, t.id, group)).map(|t| t.id).collect()
}

/// Whether a track follows its group: it goes to Main (which the engine
/// reads as "the enclosing group") or straight to the group it was in. A
/// track routed somewhere of its own (hardware, nowhere) keeps that.
fn follows_group(output: Output, parent: Option<TrackId>) -> bool {
    match output {
        Output::Main => true,
        Output::Track(t) => Some(t) == parent,
        _ => false,
    }
}

/// Where a group goes: its own parent group, else Main.
fn bus_output(p: &Project, parent: Option<TrackId>) -> Output {
    match parent.and_then(|g| p.track(g)) {
        Some(g) if g.kind == TrackKind::Group => Output::Track(g.id),
        _ => Output::Main,
    }
}

/// The commands that put `ids` in a new group `group` (Cmd+G). Tracks that
/// cannot be grouped (Main, returns, VCAs) and tracks already inside another
/// selected track are left out. The group takes the place and colour of the
/// first track; the others are moved under it, keeping their order and
/// their own members. `None` when nothing can be grouped.
pub fn group_commands(p: &Project, ids: &[TrackId], group: TrackId) -> Option<Vec<Command>> {
    let groupable = |t: &Track| t.kind.has_clips() || matches!(t.kind, TrackKind::Group | TrackKind::Folder);
    let mut roots: Vec<(usize, TrackId)> = ids
        .iter()
        .filter_map(|&id| Some((p.track_index(id)?, id)))
        .filter(|&(i, _)| groupable(&p.tracks[i]))
        .collect();
    roots.sort_unstable();
    roots.dedup();
    let picked: Vec<TrackId> = roots.iter().map(|r| r.1).collect();
    roots.retain(|&(_, id)| !ancestors(p, id).any(|a| picked.contains(&a)));
    let &(index, first) = roots.first()?;
    let first = p.track(first)?;

    let parent = first.parent;
    let mut g = Track::new(group, TrackKind::Group, "Group", first.color);
    g.parent = parent;
    g.output = bus_output(p, parent);
    g.sends = first.sends.clone();
    let mut cmds = vec![Command::AddTrack { track: Box::new(g), index }];

    // Work on a scratch copy so every move sees the indices the ones before it left.
    let mut scratch = p.clone();
    Command::AddTrack { track: Box::new(Track::new(group, TrackKind::Group, "", first.color)), index }.apply(&mut scratch).ok()?;
    let mut pos = index + 1;
    for &(_, root) in &roots {
        let subtree: Vec<TrackId> = std::iter::once(root).chain(descendants(p, root)).collect();
        for id in subtree {
            let from = scratch.track_index(id)?;
            if from != pos {
                let mv = Command::MoveTrack { id, to: pos };
                mv.clone().apply(&mut scratch).ok()?;
                cmds.push(mv);
            }
            pos += 1;
        }
        let t = p.track(root)?;
        cmds.push(Command::SetTrack { id: root, change: TrackProp::Parent(Some(group)) });
        if follows_group(t.output, t.parent) {
            cmds.push(Command::SetTrack { id: root, change: TrackProp::Output(Output::Track(group)) });
        }
    }
    Some(cmds)
}

/// The commands that dissolve `group` (Cmd+Shift+G): its members move up to
/// the group's parent and send their audio where the group sent its own.
/// `None` when `group` is not a group.
pub fn ungroup_commands(p: &Project, group: TrackId) -> Option<Vec<Command>> {
    let g = p.track(group).filter(|t| matches!(t.kind, TrackKind::Group | TrackKind::Folder))?;
    let mut cmds = Vec::new();
    for t in p.tracks.iter().filter(|t| t.parent == Some(group)) {
        cmds.push(Command::SetTrack { id: t.id, change: TrackProp::Parent(g.parent) });
        if follows_group(t.output, Some(group)) {
            cmds.push(Command::SetTrack { id: t.id, change: TrackProp::Output(bus_output(p, g.parent)) });
        }
    }
    // Sends that fed the group would point nowhere.
    for t in p.tracks.iter().filter(|t| t.sends.iter().any(|s| s.target == group)) {
        let sends = t.sends.iter().filter(|s| s.target != group).cloned().collect();
        cmds.push(Command::SetTrack { id: t.id, change: TrackProp::Sends(sends) });
    }
    cmds.push(Command::RemoveTrack { id: group });
    Some(cmds)
}

/// The spans `[start, end)` in ticks where the tracks inside `group` have
/// arrangement clips, merged per track: one entry per member that has any,
/// with that track's id. What a group's lane shows in Live.
pub fn member_spans(p: &Project, group: TrackId) -> Vec<(TrackId, Vec<(crate::Tick, crate::Tick)>)> {
    let mut out = Vec::new();
    for id in descendants(p, group) {
        let Some(t) = p.track(id).filter(|t| t.kind.has_clips()) else { continue };
        let mut spans: Vec<(crate::Tick, crate::Tick)> = t
            .arrangement
            .iter()
            .filter_map(|pl| Some((pl.start, pl.start + p.clips.get(&pl.clip)?.length)))
            .filter(|(a, b)| b > a)
            .collect();
        if spans.is_empty() {
            continue;
        }
        spans.sort_unstable();
        let mut merged: Vec<(crate::Tick, crate::Tick)> = Vec::with_capacity(spans.len());
        for (a, b) in spans {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        out.push((id, merged));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{History, Origin};

    fn project() -> (Project, Vec<TrackId>) {
        let mut p = Project::new("t");
        let mut ids = Vec::new();
        for i in 0..4 {
            let id = p.alloc_track_id();
            let t = Track::new(id, TrackKind::Audio, format!("A{i}"), Color::palette(i));
            let at = p.tracks.len() - 1; // before Main
            Command::AddTrack { track: Box::new(t), index: at }.apply(&mut p).unwrap();
            ids.push(id);
        }
        (p, ids)
    }

    fn names(p: &Project) -> Vec<String> {
        p.content_tracks().map(|t| format!("{}{}", "  ".repeat(depth(p, t.id)), t.name)).collect()
    }

    #[test]
    fn grouping_gathers_the_selection_under_a_new_group_and_undoes() {
        let (mut p, ids) = project();
        let before = p.clone();
        let g = p.alloc_track_id();
        let cmds = group_commands(&p, &[ids[3], ids[1]], g).unwrap();
        let mut h = History::default();
        h.apply(&mut p, "Group", Origin::User, cmds).unwrap();
        assert_eq!(names(&p), ["A0", "Group", "  A1", "  A3", "A2"]);
        assert_eq!(p.track(ids[1]).unwrap().output, Output::Track(g));
        assert_eq!(p.track(g).unwrap().output, Output::Main);
        assert_eq!(p.track(g).unwrap().color, p.track(ids[1]).unwrap().color);
        h.undo(&mut p).unwrap();
        assert_eq!(p.tracks, before.tracks);
    }

    #[test]
    fn groups_nest_and_route_to_the_outer_group() {
        let (mut p, ids) = project();
        let inner = p.alloc_track_id();
        for c in group_commands(&p, &[ids[1], ids[2]], inner).unwrap() {
            c.apply(&mut p).unwrap();
        }
        let outer = p.alloc_track_id();
        for c in group_commands(&p, &[ids[0], inner], outer).unwrap() {
            c.apply(&mut p).unwrap();
        }
        assert_eq!(names(&p), ["Group", "  A0", "  Group", "    A1", "    A2", "A3"]);
        assert_eq!(p.track(inner).unwrap().output, Output::Track(outer));
        assert_eq!(descendants(&p, outer), vec![ids[0], inner, ids[1], ids[2]]);
        assert_eq!(depth(&p, ids[2]), 2);
        // Folding the outer group hides everything inside it.
        Command::SetTrack { id: outer, change: TrackProp::Collapsed(true) }.apply(&mut p).unwrap();
        assert!(hidden(&p, ids[2]) && hidden(&p, inner) && !hidden(&p, outer) && !hidden(&p, ids[3]));
        // A track inside a selected group is not grouped twice.
        let g = p.alloc_track_id();
        let cmds = group_commands(&p, &[inner, ids[1]], g).unwrap();
        assert_eq!(cmds.iter().filter(|c| matches!(c, Command::SetTrack { change: TrackProp::Parent(_), .. })).count(), 1);
    }

    #[test]
    fn ungrouping_hands_the_members_to_the_parent() {
        let (mut p, ids) = project();
        let inner = p.alloc_track_id();
        for c in group_commands(&p, &[ids[1], ids[2]], inner).unwrap() {
            c.apply(&mut p).unwrap();
        }
        let outer = p.alloc_track_id();
        for c in group_commands(&p, &[inner], outer).unwrap() {
            c.apply(&mut p).unwrap();
        }
        for c in ungroup_commands(&p, inner).unwrap() {
            c.apply(&mut p).unwrap();
        }
        assert!(p.track(inner).is_none());
        let a1 = p.track(ids[1]).unwrap();
        assert_eq!((a1.parent, a1.output), (Some(outer), Output::Track(outer)));
        for c in ungroup_commands(&p, outer).unwrap() {
            c.apply(&mut p).unwrap();
        }
        let a1 = p.track(ids[1]).unwrap();
        assert_eq!((a1.parent, a1.output), (None, Output::Main));
        assert!(ungroup_commands(&p, ids[0]).is_none());
        assert!(group_commands(&p, &[p.main_track().id], TrackId(999)).is_none());
    }

    #[test]
    fn member_spans_merge_overlapping_clips() {
        let (mut p, ids) = project();
        let g = p.alloc_track_id();
        for c in group_commands(&p, &[ids[0]], g).unwrap() {
            c.apply(&mut p).unwrap();
        }
        let src = p.alloc_source_id();
        Command::AddSource { id: src, source: Box::new(Source::Notes(Default::default())) }.apply(&mut p).unwrap();
        for (start, len) in [(0, 100), (50, 100), (400, 10)] {
            let clip = p.alloc_clip_id();
            Command::AddClip { id: clip, clip: Box::new(Clip::new("c", Color::palette(0), src, len)) }.apply(&mut p).unwrap();
            let id = p.alloc_placement_id();
            Command::PlaceClip { track: ids[0], placement: Placement { id, clip, start, lane: 0 } }.apply(&mut p).unwrap();
        }
        assert_eq!(member_spans(&p, g), vec![(ids[0], vec![(0, 150), (400, 410)])]);
    }
}
