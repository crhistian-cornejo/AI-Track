//! Working on a clip directly in the arrangement lanes, as in Live: the
//! waveform as it will sound, a fade handle in each top corner (drag it
//! inwards for the fade's length, drag the dot on its slope to bend it,
//! double-click to reset), both edges (the left one trims or reveals the
//! start, the right one resizes and loops), and Ctrl+drag to drag copies.
//!
//! - [`geom`]: where everything is and what is under the pointer; which
//!   source frames a pixel column shows. Plain data.
//! - [`edit`]: what the gestures do to the project, as commands. Headless.
//! - [`draw`]: the waveform, the fades and their handles.
//! - here: the mouse. `views::arrangement` hit-tests the lane and calls
//!   [`press`]; the drags run through `Workspace::drag_move` as
//!   [`Drag::ClipShape`] and [`Drag::ClipCopy`]. Holding Alt bypasses
//!   snapping; Escape gives the drag up.

pub mod draw;
pub mod edit;
pub mod geom;

pub use geom::{ClipBox, Zone};

use crate::editing::GridView;
use crate::state::BottomTab;
use crate::views::arrangement::LaneRow;
use crate::views::sample_editor::geom::curve_through;
use crate::workspace::{Drag, Workspace};
use aitrack_model::ops::MIN_CLIP_LEN;
use aitrack_model::*;
use edit::{Copies, Edit, Origin};
use gpui::{Context, CursorStyle, MouseDownEvent, MouseMoveEvent, Pixels, Point};

/// The pointer moves this far before a press becomes a drag.
const SLOP: f32 = 3.;

/// A drag on a fade handle, a fade's slope or the left edge of a clip.
#[derive(Clone, Debug)]
pub struct ShapeDrag {
    zone: Zone,
    origin: Origin,
    /// The pointer at mouse-down, and the tick under it.
    from: Point<Pixels>,
    grab_tick: Tick,
    /// The pointer's distance below the slope handle at mouse-down, and the
    /// clip's rectangle then (the gain scale a slope drag reads).
    grab_y: f32,
    bx: ClipBox,
    moved: bool,
    /// Whether the project changed (Escape then undoes it).
    applied: bool,
}

/// Ctrl+drag on a clip: copies of it (or of the selection it is part of)
/// follow the pointer. A press that never moves is a Ctrl+click instead.
#[derive(Clone, Debug)]
pub struct CopyDrag {
    /// The clip under the pointer at mouse-down, its track and its start.
    under: PlacementId,
    home: TrackId,
    start: Tick,
    from: Point<Pixels>,
    grab_tick: Tick,
    /// Made once the pointer has moved.
    copies: Option<Copies>,
    /// Lanes the copies are away from their originals.
    rows: i32,
}

/// Canvas-local pixels of a window position.
fn local(ws: &Workspace, pos: Point<Pixels>) -> (f32, f32) {
    let b = ws.regions.arrange.get();
    (f32::from(pos.x - b.origin.x), f32::from(pos.y - b.origin.y))
}

/// The rectangle of a placement of `row`'s track, as the lanes paint it.
pub fn clip_box(ws: &Workspace, row: &LaneRow, placement: &Placement) -> Option<ClipBox> {
    let clip = ws.s.project.clips.get(&placement.clip)?;
    let audio = matches!(ws.s.project.sources.get(&clip.source), Some(Source::Audio(_)));
    let view = ws.s.arrange;
    Some(ClipBox::of(clip, audio, placement.start, row.top - view.scroll_y, row.h, view.px_per_beat / PPQ as f32, view.scroll_x))
}

/// Left button down on a clip that was just selected: a fade handle or the
/// left edge starts shaping it, the right edge resizes it, anywhere else
/// moves it; a double-click resets a fade or opens the clip's editor.
pub fn press(ws: &mut Workspace, row: &LaneRow, placement: PlacementId, tick: Tick, e: &MouseDownEvent, cx: &mut Context<Workspace>) {
    let Some((track, pl, length)) = ws.s.placement(placement) else { return };
    let Some(bx) = clip_box(ws, row, &pl) else { return };
    let (x, y) = local(ws, e.position);
    // The lane's margin above and below a clip still moves it.
    let zone = bx.hit(x, y).unwrap_or(Zone::Body);
    let twice = e.click_count >= 2;
    match zone {
        Zone::FadeIn | Zone::FadeOut | Zone::FadeInCurve | Zone::FadeOutCurve if twice => {
            let fade_in = matches!(zone, Zone::FadeIn | Zone::FadeInCurve);
            edit::reset_fade(&mut ws.s, pl.clip, fade_in, matches!(zone, Zone::FadeInCurve | Zone::FadeOutCurve));
            cx.notify();
        }
        // The Clip tab: piano roll for notes, sample editor for audio.
        _ if twice => ws.set_tab(BottomTab::PianoRoll, cx),
        Zone::End => ws.begin_drag(Drag::ClipResize { clip: pl.clip, length, grab_tick: tick }, cx),
        Zone::Body => ws.begin_drag(Drag::Clip { track, placement, start: pl.start, grab_tick: tick, applied: (pl.start, track) }, cx),
        Zone::FadeIn | Zone::FadeOut | Zone::FadeInCurve | Zone::FadeOutCurve | Zone::Start => {
            let Some(origin) = Origin::take(&ws.s, placement) else { return };
            let handle_y = bx.slope(matches!(zone, Zone::FadeInCurve)).map_or(y, |p| p.1);
            ws.begin_drag(Drag::ClipShape(Box::new(ShapeDrag { zone, origin, from: e.position, grab_tick: tick, grab_y: y - handle_y, bx, moved: false, applied: false })), cx);
        }
    }
}

/// Ctrl + left button down on a clip (from `editing::gestures::lane_down`).
pub fn begin_copy(ws: &mut Workspace, placement: PlacementId, tick: Tick, e: &MouseDownEvent, cx: &mut Context<Workspace>) {
    let Some((home, pl, _)) = ws.s.placement(placement) else { return };
    ws.begin_drag(Drag::ClipCopy(CopyDrag { under: placement, home, start: pl.start, from: e.position, grab_tick: tick, copies: None, rows: 0 }), cx);
}

fn past_slop(from: Point<Pixels>, e: &MouseMoveEvent) -> bool {
    f32::from(e.position.x - from.x).hypot(f32::from(e.position.y - from.y)) >= SLOP
}

/// Mouse move during [`Drag::ClipShape`] or [`Drag::ClipCopy`].
pub fn drag_move(ws: &mut Workspace, drag: &mut Drag, e: &MouseMoveEvent) {
    let (tick, row) = ws.arrange_pos(e.position);
    let free = e.modifiers.alt;
    match drag {
        Drag::ClipShape(d) => {
            if !d.moved && !past_slop(d.from, e) {
                return;
            }
            d.moved = true;
            let o = &d.origin;
            // The handle lands on the grid (not the amount it moved).
            let snap = |at: Tick| ws.s.snap_tick(GridView::Arrange, at + tick - d.grab_tick, free);
            let edit = match d.zone {
                Zone::FadeIn => Edit::FadeIn(snap(o.start + o.clip.fade_in.length) - o.start),
                Zone::FadeOut => Edit::FadeOut(o.end() - snap(o.end() - o.clip.fade_out.length)),
                Zone::FadeInCurve | Zone::FadeOutCurve => {
                    // The slope passes under the pointer.
                    let curve = curve_through(d.bx.gain_at_y(local(ws, e.position).1 - d.grab_y));
                    if d.zone == Zone::FadeInCurve { Edit::FadeInCurve(curve) } else { Edit::FadeOutCurve(curve) }
                }
                Zone::Start => {
                    // No shorter than a grid step (or than it was, when it was shorter).
                    let step = ws.s.snap_step(GridView::Arrange).filter(|_| !free).unwrap_or(MIN_CLIP_LEN);
                    Edit::Start(snap(o.start).clamp(0, o.end() - step.min(o.clip.length)))
                }
                Zone::End | Zone::Body => return,
            };
            d.applied |= edit::apply(&mut ws.s, &d.origin, edit);
        }
        Drag::ClipCopy(d) => {
            if d.copies.is_none() {
                if !past_slop(d.from, e) {
                    return;
                }
                d.copies = Copies::make(&mut ws.s, d.under);
            }
            let Some(copies) = &d.copies else { return };
            let dt = ws.s.snap_tick(GridView::Arrange, d.start + tick - d.grab_tick, free).max(0) - d.start;
            // To the lane under the pointer when every copy fits there,
            // else where the copies already are.
            let lanes = ws.s.lane_tracks();
            let lane = |t: TrackId| lanes.iter().position(|l| *l == t).map(|i| i as i32);
            let rows = row.and_then(|t| Some(lane(t)? - lane(d.home)?)).filter(|r| ws.s.shift_tracks(&copies.origin, *r).is_some()).unwrap_or(d.rows);
            copies.move_to(&mut ws.s, dt, rows);
            d.rows = rows;
        }
        _ => {}
    }
}

/// Mouse-up of a copy-drag: the copies stay and are the selection; a press
/// that never moved toggles the clip in the selection, as Ctrl+click does.
pub fn copy_end(ws: &mut Workspace, d: CopyDrag) {
    match d.copies {
        Some(copies) => {
            copies.finish(&mut ws.s);
        }
        None => ws.s.click_clip(d.under, true),
    }
}

/// Mouse-up of a fade or trim drag: a trimmed notes clip settles its notes.
pub fn shape_end(ws: &mut Workspace, d: &ShapeDrag) {
    if d.zone == Zone::Start && d.applied {
        edit::finish_trim(&mut ws.s, &d.origin);
    }
}

/// Escape during one of these drags: the clip goes back to how it was, the
/// copies go away. `false` when no such drag is in flight.
pub fn cancel_drag(ws: &mut Workspace) -> bool {
    match ws.drag.take() {
        Some(Drag::ClipShape(d)) => {
            ws.s.seal();
            if d.applied {
                ws.s.undo();
            }
            true
        }
        Some(Drag::ClipCopy(d)) => {
            if let Some(copies) = d.copies {
                copies.cancel(&mut ws.s);
            }
            true
        }
        other => {
            ws.drag = other;
            false
        }
    }
}

/// The clip a drag in flight holds and where it holds it, so the lanes keep
/// that target lit and its pointer shown wherever the pointer goes.
pub fn held(ws: &Workspace) -> Option<(PlacementId, Zone)> {
    match ws.drag.as_ref()? {
        Drag::ClipShape(d) => Some((d.origin.placement, d.zone)),
        Drag::ClipResize { clip, .. } => {
            let (_, id) = ws.s.selected_placement?;
            ws.s.placement(id).filter(|(_, pl, _)| pl.clip == *clip).map(|_| (id, Zone::End))
        }
        Drag::Clip { placement, .. } => Some((*placement, Zone::Body)),
        _ => None,
    }
}

/// Whether copies are being dragged.
pub fn copying(ws: &Workspace) -> bool {
    matches!(&ws.drag, Some(Drag::ClipCopy(d)) if d.copies.is_some())
}

/// The pointer that shows what a press on `zone` does (or is doing).
pub fn cursor_for(zone: Option<Zone>, copying: bool) -> CursorStyle {
    match zone {
        _ if copying => CursorStyle::DragCopy,
        Some(Zone::Start | Zone::End) => CursorStyle::ResizeLeftRight,
        Some(Zone::FadeIn | Zone::FadeOut) => CursorStyle::PointingHand,
        Some(Zone::FadeInCurve | Zone::FadeOutCurve) => CursorStyle::ResizeUpDown,
        Some(Zone::Body) | None => CursorStyle::Arrow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_has_its_pointer() {
        for edge in [Zone::Start, Zone::End] {
            assert_eq!(cursor_for(Some(edge), false), CursorStyle::ResizeLeftRight);
        }
        for handle in [Zone::FadeIn, Zone::FadeOut] {
            assert_eq!(cursor_for(Some(handle), false), CursorStyle::PointingHand);
        }
        for slope in [Zone::FadeInCurve, Zone::FadeOutCurve] {
            assert_eq!(cursor_for(Some(slope), false), CursorStyle::ResizeUpDown);
        }
        assert_eq!(cursor_for(Some(Zone::Body), false), CursorStyle::Arrow);
        assert_eq!(cursor_for(None, false), CursorStyle::Arrow);
        assert_eq!(cursor_for(Some(Zone::Body), true), CursorStyle::DragCopy);
    }
}
