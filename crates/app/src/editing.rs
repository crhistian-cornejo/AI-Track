//! Editing in the arrangement and the piano roll: what is selected, where
//! the insert marker is, the clipboard and the grid the gestures snap to.
//!
//! All of it lives in one [`EditState`] on [`AppState`]. The commands are
//! methods on `AppState` and never touch GPUI, so they are tested headlessly;
//! the mouse gestures and the context menus that call them are in
//! [`gestures`] and [`menus`].
//!
//! The selection follows Live: clicking empty lane space puts the **insert
//! marker** there, dragging makes a **time selection** across the tracks
//! dragged over, clicking clips builds a **clip selection** (and a click
//! that does not drag the clip puts the marker where it was clicked). Cut,
//! Copy, Duplicate and Delete act on the time selection when there is one,
//! else on the selected clips; Paste lands on the insert marker, and Split
//! cuts at it (at the playhead while playing).

mod clipboard;
pub mod gestures;
mod groups;
pub mod menus;
pub mod overlay;
mod timeline;

#[cfg(test)]
mod tests;

pub use clipboard::Clipboard;

use crate::grid::{self, GridSpec};
use crate::state::{AppState, ArrangeView, KeyEdit, Snap};
use aitrack_model::*;
use std::collections::BTreeSet;

/// The view whose zoom decides the adaptive grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridView {
    Arrange,
    Piano,
}

/// Which editor the editing keys act on: the one clicked last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Area {
    #[default]
    Arrange,
    Piano,
}

/// A time range across some tracks of the arrangement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeSel {
    pub start: Tick,
    pub end: Tick,
    /// The tracks it covers, top to bottom.
    pub tracks: Vec<TrackId>,
    /// Where the drag began; Shift+click extends from here.
    pub anchor: (Tick, TrackId),
}

/// Editing state that is not part of the project.
#[derive(Clone, Debug)]
pub struct EditState {
    pub area: Area,
    /// The selected arrangement clips. Read it through
    /// [`AppState::selected_clip_ids`], which also honours a plain write to
    /// `AppState::selected_placement`.
    pub clips: BTreeSet<PlacementId>,
    pub time: Option<TimeSel>,
    /// The insert marker: where playback starts and Paste lands.
    pub marker: Tick,
    /// The insert marker of the piano roll, in clip time.
    pub piano_marker: Tick,
    pub clipboard: Option<Clipboard>,
    /// Adaptive grid density, -2 (narrowest) ..= 2 (widest).
    pub density: i8,
    pub triplet: bool,
    /// The snap mode Ctrl+4 returns to after switching snapping off.
    pub snap_before: Snap,
    /// Keep the playhead in view while playing.
    pub follow: bool,
    /// Views to return to after Zoom to Selection.
    pub zoom_stack: Vec<ArrangeView>,
    /// Where each clip of the drag in flight was when it began: placement,
    /// track and start.
    pub drag_origin: Vec<(PlacementId, TrackId, Tick)>,
    /// Where the clip under the pointer was pressed: the insert marker goes
    /// there when the press ends without having dragged the clip.
    pub pressed: Option<Tick>,
    /// The tracks picked with Cmd/Shift+click on their headers. Read it
    /// through [`AppState::selected_tracks`], which also honours a plain
    /// write to `AppState::selected_track`.
    pub tracks: BTreeSet<TrackId>,
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            area: Area::Arrange,
            clips: BTreeSet::new(),
            time: None,
            marker: 0,
            piano_marker: 0,
            clipboard: None,
            density: 0,
            triplet: false,
            snap_before: Snap::Adaptive,
            follow: false,
            zoom_stack: Vec::new(),
            drag_origin: Vec::new(),
            pressed: None,
            tracks: BTreeSet::new(),
        }
    }
}

/// `t` rounded up to a multiple of `step`.
pub(crate) fn ceil_to(t: Tick, step: Tick) -> Tick {
    let step = step.max(1);
    (t + step - 1).div_euclid(step) * step
}

// ───────────────────────────── Grid ─────────────────────────────

impl AppState {
    fn signature(&self) -> TimeSignature {
        self.project.tempo.signature_at(0)
    }

    fn zoom_of(&self, view: GridView) -> f32 {
        match view {
            GridView::Arrange => self.arrange.px_per_beat,
            GridView::Piano => self.piano.px_per_beat,
        }
    }

    /// The grid `view` snaps to: adaptive to its zoom, or the fixed value
    /// picked in the Snap menu. Bars and beats follow the time signature.
    pub fn grid_spec(&self, view: GridView) -> GridSpec {
        let adaptive = grid::adaptive(self.zoom_of(view), self.signature(), self.ed.density, self.ed.triplet);
        match self.snap {
            Snap::Off | Snap::Adaptive => adaptive,
            Snap::Bar => GridSpec { step: adaptive.bar, ..adaptive },
            fixed => GridSpec { step: fixed.ticks(), ..adaptive },
        }
    }

    /// The grid `view` draws: [`Self::grid_spec`], unless a fixed step is
    /// too fine to draw at this zoom, where the adaptive one stands in.
    pub fn draw_spec(&self, view: GridView) -> GridSpec {
        let spec = self.grid_spec(view);
        if spec.step as f32 * self.zoom_of(view) / PPQ as f32 >= 5. {
            spec
        } else {
            grid::adaptive(self.zoom_of(view), self.signature(), 0, false)
        }
    }

    /// The step gestures in `view` snap to; `None` while snapping is off.
    pub fn snap_step(&self, view: GridView) -> Option<Tick> {
        (self.snap != Snap::Off).then(|| self.grid_spec(view).step.max(1))
    }

    /// `t` on the nearest grid line of `view`. `free` (Alt held) and
    /// snapping switched off leave it where it is.
    pub fn snap_tick(&self, view: GridView, t: Tick, free: bool) -> Tick {
        match self.snap_step(view).filter(|_| !free) {
            Some(step) => (t + step / 2).div_euclid(step) * step,
            None => t,
        }
    }

    /// `t` on the grid line of `view` at or before it.
    pub fn snap_floor(&self, view: GridView, t: Tick) -> Tick {
        match self.snap_step(view) {
            Some(step) => t.div_euclid(step) * step,
            None => t,
        }
    }

    /// What the Snap button shows: "Off", "1/16" for a fixed grid, or the
    /// step the adaptive grid has at this zoom.
    pub fn snap_label(&self, view: GridView) -> String {
        match self.snap {
            Snap::Off => "Off".into(),
            Snap::Adaptive => format!("Auto {}", self.grid_spec(view).label()),
            fixed => fixed.label().into(),
        }
    }

    /// Ctrl+1 / Ctrl+2: a narrower (`-1`) or wider (`1`) grid. The adaptive
    /// grid changes density; a fixed one moves to the next note value.
    pub fn step_grid(&mut self, dir: i8) -> bool {
        const FIXED: [Snap; 5] = [Snap::Bar, Snap::Beat, Snap::Eighth, Snap::Sixteenth, Snap::ThirtySecond];
        match self.snap {
            Snap::Off => false,
            Snap::Adaptive => {
                let next = (self.ed.density + dir).clamp(-2, 2);
                let changed = next != self.ed.density;
                self.ed.density = next;
                changed
            }
            fixed => {
                let at = FIXED.iter().position(|s| *s == fixed).unwrap_or(3) as i32;
                let next = FIXED[(at - i32::from(dir)).clamp(0, FIXED.len() as i32 - 1) as usize];
                let changed = next != fixed;
                self.snap = next;
                changed
            }
        }
    }

    /// Ctrl+3: straight or triplet steps (on the adaptive grid).
    pub fn toggle_triplet(&mut self) -> bool {
        self.ed.triplet = !self.ed.triplet;
        if self.snap != Snap::Off {
            self.snap = Snap::Adaptive;
        }
        self.ed.triplet
    }

    /// Ctrl+4: snapping off, or back to the mode it had.
    pub fn toggle_snap(&mut self) -> bool {
        if self.snap == Snap::Off {
            self.snap = if self.ed.snap_before == Snap::Off { Snap::Adaptive } else { self.ed.snap_before };
        } else {
            self.ed.snap_before = self.snap;
            self.snap = Snap::Off;
        }
        self.snap != Snap::Off
    }
}

// ───────────────────────────── Selection ─────────────────────────────

impl AppState {
    /// A placement by id: its track, the placement and the length of its clip.
    pub fn placement(&self, id: PlacementId) -> Option<(TrackId, Placement, Tick)> {
        self.project.tracks.iter().find_map(|t| {
            let pl = t.arrangement.iter().find(|p| p.id == id)?;
            Some((t.id, pl.clone(), self.project.clips.get(&pl.clip)?.length))
        })
    }

    /// The tracks with a lane on screen, top to bottom.
    pub fn lane_tracks(&self) -> Vec<TrackId> {
        crate::views::arrangement::rows(self).into_iter().map(|r| r.track).collect()
    }

    /// The selected clips. Code that only knows `selected_placement` and
    /// writes it directly selects that one clip.
    pub fn selected_clip_ids(&self) -> BTreeSet<PlacementId> {
        match self.selected_placement {
            Some((_, id)) if !self.ed.clips.contains(&id) => [id].into(),
            _ => self.ed.clips.clone(),
        }
    }

    /// The selected clips in track order, then by start.
    pub fn clip_selection(&self) -> Vec<(TrackId, Placement, Tick)> {
        let mut out: Vec<_> = self.selected_clip_ids().into_iter().filter_map(|id| self.placement(id)).collect();
        out.sort_by_key(|(t, pl, _)| (self.project.track_index(*t), pl.start));
        out
    }

    /// The time selection, when it covers some time on some track.
    pub fn time_selection(&self) -> Option<&TimeSel> {
        self.ed.time.as_ref().filter(|s| s.end > s.start && !s.tracks.is_empty())
    }

    /// The time the selection covers: the time selection, else the selected clips.
    pub fn selection_span(&self) -> Option<(Tick, Tick)> {
        if let Some(sel) = self.time_selection() {
            return Some((sel.start, sel.end));
        }
        let clips = self.clip_selection();
        let start = clips.iter().map(|(_, pl, _)| pl.start).min()?;
        Some((start, clips.iter().map(|(_, pl, len)| pl.start + len).max()?))
    }

    /// Select exactly these clips; `last` is the one clicked.
    pub fn select_clips(&mut self, ids: impl IntoIterator<Item = PlacementId>, last: Option<PlacementId>) {
        self.ed.clips = ids.into_iter().collect();
        self.ed.time = None;
        let last = last.filter(|id| self.ed.clips.contains(id)).or(self.ed.clips.iter().next().copied());
        match last.and_then(|id| self.placement(id)) {
            Some((track, pl, _)) => {
                self.selected_placement = Some((track, pl.id));
                self.select_clip(Some(pl.clip));
            }
            None => self.selected_placement = None,
        }
    }

    /// A click on a clip. A plain click selects it alone (a clip that is
    /// already part of the selection keeps the others, so the group can be
    /// dragged); `toggle` (Shift or Ctrl) adds it or takes it out.
    pub fn click_clip(&mut self, id: PlacementId, toggle: bool) {
        self.ed.area = Area::Arrange;
        let mut ids = self.selected_clip_ids();
        if toggle {
            if !ids.remove(&id) {
                ids.insert(id);
            }
            let last = ids.contains(&id).then_some(id);
            self.select_clips(ids, last);
        } else if ids.contains(&id) {
            self.select_clips(ids, Some(id));
        } else {
            self.select_clips([id], Some(id));
        }
    }

    /// A plain press on a clip at `tick`: it is selected as [`Self::click_clip`]
    /// does and can be dragged with the rest of the selection; where it was
    /// pressed (on the grid unless `free`) is kept for [`Self::release_clip`].
    pub fn press_clip(&mut self, id: PlacementId, tick: Tick, free: bool) {
        self.click_clip(id, false);
        // Dragging one selected clip moves all of them.
        self.ed.drag_origin = self.clip_selection().into_iter().map(|(t, pl, _)| (pl.id, t, pl.start)).collect();
        self.ed.pressed = Some(self.snap_tick(GridView::Arrange, tick.max(0), free));
    }

    /// The end of a press on a clip. A press that never `dragged` the clip
    /// was a click: the insert marker goes where it was pressed, and the
    /// clip stays selected. Returns the marker's new place.
    pub fn release_clip(&mut self, dragged: bool) -> Option<Tick> {
        let at = self.ed.pressed.take().filter(|_| !dragged)?;
        self.ed.marker = at;
        Some(at)
    }

    /// The length the clip of placement `id` can grow to before it reaches
    /// the next clip on its track and lane; `None` when nothing follows it.
    /// `length` is how long it was when the drag began, so a clip that
    /// already overlaps the next one is not cut back.
    pub fn room_after(&self, id: PlacementId, length: Tick) -> Option<Tick> {
        let (track, pl, _) = self.placement(id)?;
        let next = self.project.track(track)?.arrangement.iter().filter(|x| x.id != id && x.lane == pl.lane && x.start >= pl.start + length).map(|x| x.start).min()?;
        Some(next - pl.start)
    }

    /// Drop the clip and time selection (the marker stays).
    pub fn clear_arrange_selection(&mut self) {
        self.ed.clips.clear();
        self.ed.time = None;
        self.selected_placement = None;
    }

    /// Put the insert marker at `tick` on `track`, dropping the selection.
    pub fn set_marker(&mut self, tick: Tick, track: Option<TrackId>) {
        self.ed.area = Area::Arrange;
        self.clear_arrange_selection();
        self.ed.marker = tick.max(0);
        if track.is_some() {
            self.selected_track = track;
        }
    }

    /// The time selection between `anchor` and `to` (tick, track), covering
    /// the lanes between the two tracks. The marker sits at its start.
    pub fn set_time_selection(&mut self, anchor: (Tick, TrackId), to: (Tick, TrackId)) {
        let lanes = self.lane_tracks();
        let index = |t: TrackId| lanes.iter().position(|l| *l == t);
        let (Some(a), Some(b)) = (index(anchor.1), index(to.1)) else { return };
        let (start, end) = (anchor.0.min(to.0).max(0), anchor.0.max(to.0).max(0));
        self.ed.area = Area::Arrange;
        self.ed.clips.clear();
        self.selected_placement = None;
        self.ed.marker = start;
        self.ed.time = (end > start).then(|| TimeSel { start, end, tracks: lanes[a.min(b)..=a.max(b)].to_vec(), anchor });
    }

    /// Shift+click: stretch the time selection (or start one at the marker)
    /// to `tick` on `track`.
    pub fn extend_time_selection(&mut self, tick: Tick, track: TrackId) {
        let anchor = match &self.ed.time {
            Some(sel) => sel.anchor,
            None => (self.ed.marker, self.selected_track.filter(|t| self.lane_tracks().contains(t)).unwrap_or(track)),
        };
        self.set_time_selection(anchor, (tick, track));
    }

    /// After undo, redo or a delete: forget what no longer exists.
    pub(crate) fn prune_edit_selection(&mut self) {
        let live: BTreeSet<PlacementId> = self.project.tracks.iter().flat_map(|t| t.arrangement.iter().map(|p| p.id)).collect();
        self.ed.clips.retain(|id| live.contains(id));
        if let Some(sel) = &mut self.ed.time {
            sel.tracks.retain(|t| self.project.tracks.iter().any(|x| x.id == *t));
            if sel.tracks.is_empty() {
                self.ed.time = None;
            }
        }
    }

    /// Ctrl+A in the arrangement: every clip. `false` when the piano roll
    /// has the keys (its notes are selected instead).
    pub fn select_all_clips(&mut self) -> bool {
        if self.ed.area != Area::Arrange {
            return false;
        }
        let ids: Vec<PlacementId> = self.project.tracks.iter().flat_map(|t| t.arrangement.iter().map(|p| p.id)).collect();
        let last = self.selected_placement.map(|(_, id)| id);
        self.select_clips(ids, last);
        true
    }

    /// Arrow keys in the arrangement: ←/→ nudge the selected clips by the
    /// grid step (or move the insert marker when no clip is selected), ↑/↓
    /// step through the tracks.
    pub(crate) fn key_edit_arrangement(&mut self, e: KeyEdit) -> bool {
        if self.ed.area != Area::Arrange {
            return false;
        }
        let step = self.snap_step(GridView::Arrange).unwrap_or(PPQ / 4);
        match e {
            KeyEdit::Left | KeyEdit::Right => {
                let dt = if e == KeyEdit::Left { -step } else { step };
                let origin: Vec<_> = self.clip_selection().into_iter().map(|(t, pl, _)| (pl.id, t, pl.start)).collect();
                if origin.is_empty() {
                    let to = (self.ed.marker + dt).max(0);
                    let moved = to != self.ed.marker;
                    self.ed.marker = to;
                    self.ed.time = None;
                    return moved;
                }
                self.seal();
                let ok = self.drag_clips(&origin, dt, 0);
                self.seal();
                ok
            }
            KeyEdit::Up | KeyEdit::Down => {
                let lanes = self.lane_tracks();
                let at = self.selected_track.and_then(|t| lanes.iter().position(|l| *l == t));
                let next = match (at, e) {
                    (Some(i), KeyEdit::Up) => i.checked_sub(1),
                    (Some(i), _) => (i + 1 < lanes.len()).then_some(i + 1),
                    (None, _) => (!lanes.is_empty()).then_some(0),
                };
                let Some(next) = next else { return false };
                self.selected_track = Some(lanes[next]);
                true
            }
            _ => false,
        }
    }

    /// The tracks the clips of a drag land on `rows` lanes away from where
    /// they began; `None` when one of them would leave the lanes or land on
    /// a track of another kind.
    pub fn shift_tracks(&self, origin: &[(PlacementId, TrackId, Tick)], rows: i32) -> Option<Vec<TrackId>> {
        let lanes = self.lane_tracks();
        let kind = |t: TrackId| self.project.track(t).map(|t| t.kind);
        origin
            .iter()
            .map(|o| {
                let i = lanes.iter().position(|l| *l == o.1)? as i32 + rows;
                let to = *lanes.get(usize::try_from(i).ok()?)?;
                (kind(to) == kind(o.1)).then_some(to)
            })
            .collect()
    }

    /// Move the clips of a drag (or a nudge) together. `origin` is where
    /// each clip was when the gesture began: `dt` ticks and `rows` lanes
    /// away from there is where they go. A move that would put a clip on a
    /// track of another kind keeps every clip on its own track. Calls of
    /// one gesture share an undo step.
    pub fn drag_clips(&mut self, origin: &[(PlacementId, TrackId, Tick)], dt: Tick, rows: i32) -> bool {
        let Some(first) = origin.iter().map(|o| o.2).min() else { return false };
        let dt = dt.max(-first);
        let dest = self.shift_tracks(origin, rows).unwrap_or_else(|| origin.iter().map(|o| o.1).collect());
        let mut cmds = Vec::new();
        let mut changed = false;
        for ((id, _, start), to) in origin.iter().zip(&dest) {
            let Some((track, pl, _)) = self.placement(*id) else { continue };
            changed |= track != *to || pl.start != start + dt;
            cmds.push(Command::MovePlacement { track, id: *id, to_track: *to, start: start + dt, lane: pl.lane });
        }
        if !changed {
            return false;
        }
        if let [Command::MovePlacement { track, id, to_track, start, .. }] = cmds[..] {
            return self.move_placement(track, id, to_track, start);
        }
        let ok = self.edit_coalesced("clip-move", "Move clips", cmds);
        if ok && let Some((_, id)) = self.selected_placement
            && let Some((track, ..)) = self.placement(id)
        {
            self.selected_placement = Some((track, id));
            self.selected_track = Some(track);
        }
        ok
    }
}

// ── Group tracks (the GroupTracks / UngroupTracks / ToggleGroupFold actions) ──

/// Cmd+G: put the selected tracks in a new group.
pub fn group_selection(ws: &mut crate::workspace::Workspace, cx: &mut gpui::Context<crate::workspace::Workspace>) {
    if ws.s.group_tracks().is_some() {
        cx.notify();
    } else {
        ws.toast("Select the tracks to group first", cx);
    }
}

/// Cmd+Shift+G: dissolve the selected groups (or the group the selected
/// track is in).
pub fn ungroup_selection(ws: &mut crate::workspace::Workspace, cx: &mut gpui::Context<crate::workspace::Workspace>) {
    if ws.s.ungroup_tracks() {
        cx.notify();
    } else {
        ws.toast("Only groups ungroup: select a group or a track inside one", cx);
    }
}

/// Fold or unfold the selected group (or the group the selected track is in).
pub fn toggle_group_fold(ws: &mut crate::workspace::Workspace, cx: &mut gpui::Context<crate::workspace::Workspace>) {
    if ws.s.toggle_fold().is_some() {
        cx.notify();
    } else {
        ws.toast("Only groups fold: select a group or a track inside one", cx);
    }
}
