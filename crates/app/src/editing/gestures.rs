//! Mouse gestures of the arrangement lanes and the ruler: placing the
//! insert marker, dragging a time selection, building a clip selection,
//! moving and resizing clips on the grid, and opening the context menus.
//!
//! `views::arrangement` hit-tests the lane and calls in here (fade handles,
//! the left edge and Ctrl+drag copies are in `views::arrange_clips`); the drags run
//! through `Workspace::drag_move`, which hands [`Drag::Clip`],
//! [`Drag::ClipResize`] and [`Drag::TimeSel`] to [`drag_move`]. Holding Alt
//! during any of them bypasses snapping.

use super::{Area, GridView};
use crate::workspace::{Drag, MenuKind, Workspace};
use aitrack_model::*;
use gpui::{Context, MouseDownEvent, MouseMoveEvent};

/// The placement on `track` under `tick`, topmost first.
pub fn clip_at(ws: &Workspace, track: TrackId, tick: Tick) -> Option<PlacementId> {
    let p = &ws.s.project;
    p.track(track)?.arrangement.iter().rev().find(|pl| p.clips.get(&pl.clip).is_some_and(|c| tick >= pl.start && tick < pl.start + c.length)).map(|pl| pl.id)
}

/// Left button down in a lane. Returns `true` when the click is dealt with.
/// A plain click on a clip returns `false`: the clip is selected here, and
/// the caller opens it in the clip editor (double-click) or starts the
/// move / resize drag.
pub fn lane_down(ws: &mut Workspace, track: TrackId, kind: TrackKind, tick: Tick, e: &MouseDownEvent, cx: &mut Context<Workspace>) -> bool {
    ws.s.ed.area = Area::Arrange;
    ws.s.ed.pressed = None;
    let toggle = e.modifiers.shift || e.modifiers.control || e.modifiers.secondary();
    if let Some(id) = clip_at(ws, track, tick) {
        // Ctrl + drag drags copies; without moving it is the Ctrl+click below.
        if !e.modifiers.shift && (e.modifiers.control || e.modifiers.secondary()) {
            crate::views::arrange_clips::begin_copy(ws, id, tick, e, cx);
            return true;
        }
        if toggle {
            ws.s.click_clip(id, toggle);
            cx.notify();
            return true;
        }
        // Released without dragging, the insert marker goes where it was
        // clicked (see `Workspace::drag_end`).
        ws.s.press_clip(id, tick, e.modifiers.alt);
        return false;
    }
    let at = ws.s.snap_tick(GridView::Arrange, tick.max(0), e.modifiers.alt);
    if e.modifiers.shift {
        ws.s.extend_time_selection(at, track);
        cx.notify();
        return true;
    }
    if e.click_count >= 2 && kind == TrackKind::Instrument {
        // Double-click an empty MIDI lane: a one-bar clip on that bar.
        let bar = ws.s.grid_spec(GridView::Arrange).bar;
        ws.s.set_marker(tick.max(0).div_euclid(bar) * bar, Some(track));
        ws.s.insert_midi_clip();
        cx.notify();
        return true;
    }
    ws.s.set_marker(at, Some(track));
    if !ws.b.engine.transport().playing {
        ws.b.engine.locate(at);
    }
    // Dragging on from here selects time.
    ws.begin_drag(Drag::TimeSel { anchor: (at, track) }, cx);
    true
}

/// Right button down in a lane: the clip menu on a clip (which joins the
/// selection first), the lane menu on empty space.
pub fn context_down(ws: &mut Workspace, track: TrackId, tick: Tick, e: &MouseDownEvent, cx: &mut Context<Workspace>) {
    ws.s.ed.area = Area::Arrange;
    ws.s.selected_track = Some(track);
    ws.menu = None;
    match clip_at(ws, track, tick) {
        Some(id) => {
            ws.s.click_clip(id, false);
            ws.open_menu(MenuKind::ClipContext, e.position, cx);
        }
        None => {
            // Inside the time selection the menu acts on it; elsewhere the marker moves here.
            let inside = ws.s.time_selection().is_some_and(|s| s.tracks.contains(&track) && tick >= s.start && tick < s.end);
            if !inside {
                let at = ws.s.snap_tick(GridView::Arrange, tick.max(0), e.modifiers.alt);
                ws.s.set_marker(at, Some(track));
            }
            ws.open_menu(MenuKind::LaneContext, e.position, cx);
        }
    }
}

/// Left button down on the ruler (below the loop strip): the insert marker
/// and the playhead go there, and dragging scrubs.
pub fn ruler_down(ws: &mut Workspace, tick: Tick, e: &MouseDownEvent, cx: &mut Context<Workspace>) {
    let at = ws.s.snap_tick(GridView::Arrange, tick.max(0), e.modifiers.alt);
    ws.s.ed.area = Area::Arrange;
    ws.s.ed.marker = at;
    ws.s.ed.time = None;
    ws.b.engine.locate(at);
    ws.begin_drag(Drag::Scrub, cx);
}

/// Mouse move during a clip move, a clip resize or a time-selection drag.
pub fn drag_move(ws: &mut Workspace, drag: &mut Drag, e: &MouseMoveEvent) {
    let (tick, row) = ws.arrange_pos(e.position);
    let free = e.modifiers.alt;
    match drag {
        Drag::Clip { track, placement, start, grab_tick, applied } => {
            if !ws.s.ed.drag_origin.iter().any(|o| o.0 == *placement) {
                ws.s.ed.drag_origin = vec![(*placement, *track, *start)];
            }
            let origin = ws.s.ed.drag_origin.clone();
            let new_start = ws.s.snap_tick(GridView::Arrange, *start + tick - *grab_tick, free).max(0);
            // Lanes moved: to the row under the mouse when every clip fits
            // there, else where the group already is.
            let lanes = ws.s.lane_tracks();
            let lane = |t: TrackId| lanes.iter().position(|l| *l == t).map(|i| i as i32);
            let home = origin.iter().find(|o| o.0 == *placement).map_or(*track, |o| o.1);
            let rows_to = |t: TrackId| Some(lane(t)? - lane(home)?);
            let current = rows_to(applied.1).unwrap_or(0);
            let rows = row.and_then(rows_to).filter(|r| ws.s.shift_tracks(&origin, *r).is_some()).unwrap_or(current);
            if ws.s.drag_clips(&origin, new_start - *start, rows)
                && let Some((t, pl, _)) = ws.s.placement(*placement)
            {
                *applied = (pl.start, t);
            }
        }
        Drag::ClipResize { clip, length, grab_tick } => {
            // The end lands on the grid (not the amount it moved).
            let placed = ws.s.selected_placement.and_then(|(_, id)| ws.s.placement(id)).filter(|(_, pl, _)| pl.clip == *clip).map(|(_, pl, _)| pl);
            let start = placed.as_ref().map(|pl| pl.start);
            // The end stops where the next clip of the track starts.
            let room = placed.and_then(|pl| ws.s.room_after(pl.id, *length)).unwrap_or(Tick::MAX);
            let new_len = match start {
                Some(start) => ws.s.snap_tick(GridView::Arrange, start + *length + tick - *grab_tick, free) - start,
                None => *length + ws.s.snap_tick(GridView::Arrange, tick - *grab_tick, free),
            };
            // No shorter than a grid step (or than it was, when it was shorter).
            let step = ws.s.snap_step(GridView::Arrange).filter(|_| !free).unwrap_or(ops::MIN_CLIP_LEN);
            ws.s.resize_clip(*clip, new_len.max(step.min(*length)).min(room));
        }
        Drag::TimeSel { anchor } => {
            let at = ws.s.snap_tick(GridView::Arrange, tick.max(0), free);
            // Above or below the lanes the selection reaches the first or last one.
            let lanes = ws.s.lane_tracks();
            let above = e.position.y < ws.regions.arrange.get().origin.y;
            let track = row.or(if above { lanes.first().copied() } else { lanes.last().copied() }).unwrap_or(anchor.1);
            ws.s.set_time_selection(*anchor, (at, track));
        }
        _ => {}
    }
}
