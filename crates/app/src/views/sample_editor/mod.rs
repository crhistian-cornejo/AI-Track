//! The sample editor: the Clip tab of the bottom panel for an audio clip.
//!
//! The clip is edited on its waveform, the way Live's Clip View and FL's
//! sample view do it: start and end markers trim it, corner handles set the
//! fades and their curve, a volume envelope shapes the level over time, a
//! brace on the ruler sets the loop, and a cursor or a time selection is
//! where it is split or cropped. Pulling the end marker past the end of the
//! file loops the clip; its repeats are drawn one after another.
//!
//! * `geom` is the coordinate maths (axis, handle positions, hit-testing,
//!   snapping), on plain data.
//! * `edit` turns gestures into `aitrack_model::ops` commands; a drag is one
//!   undo step and always starts from a snapshot of the clip.
//! * `draw` paints the canvas.
//!
//! This file is the view state, the toolbar and the mouse and key handling.

mod draw;
pub mod edit;
pub mod geom;

use crate::actions::{ArrangeFit, ArrangeZoomIn, ArrangeZoomOut, DeleteSelection, Escape, SelectAll, SplitClip, ToggleDrawTool};
use crate::bridge::SlotState;
use crate::grid::{self, GridSpec};
use crate::icons::Icon;
use crate::state::BottomTab;
use crate::theme::*;
use crate::waveform::Status;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_model::ops::MIN_CLIP_LEN;
use aitrack_model::{AudioSource, Clip, ClipId, PPQ, Source, Tick, TimeSignature, curve_shape};
use edit::{Gesture, Placed, Snapshot};
use geom::{Axis, Hit, Layout, RULER, TimeBase};
use gpui::{
    AnyElement, Bounds, Context, CursorStyle, DispatchPhase, FocusHandle, FontWeight, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, ScrollDelta, ScrollWheelEvent, Stateful,
    StatefulInteractiveElement, Styled, Window, canvas, div, prelude::FluentBuilder, px,
};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A single sample is at most this wide.
const SAMPLE_PX: f64 = 8.;
/// The pointer has to travel this far before a press becomes a drag.
const SLOP: f32 = 3.;

/// View state of the sample editor (lives on the workspace as `sample`).
#[derive(Default)]
pub struct UiState {
    focus: Option<FocusHandle>,
    /// Bounds of the canvas, written during prepaint.
    region: Rc<Cell<Bounds<Pixels>>>,
    /// The clip the rest of this state belongs to.
    clip: Option<ClipId>,
    /// Zoom and scroll; `None` shows the whole file.
    axis: Option<Axis>,
    /// The edit cursor and the time selection, in unrolled source ticks.
    cursor: Option<Tick>,
    selection: Option<(Tick, Tick)>,
    hover: Option<Hit>,
    /// The breakpoint clicked last (Delete removes it).
    picked: Option<usize>,
    drag: Option<Drag>,
    /// Clicks on the waveform draw volume breakpoints.
    pencil: bool,
    /// Until when the file is being auditioned.
    audition: Option<Instant>,
    /// Length of the decoded file in source ticks, for files the project has no length for.
    file_len: Option<Tick>,
    /// The closest zoom, in px per beat (single samples, or the finest peaks).
    max_zoom: f64,
}

impl UiState {
    fn open(&mut self, clip: ClipId) {
        self.clip = Some(clip);
        self.axis = None;
        self.cursor = None;
        self.selection = None;
        self.hover = None;
        self.picked = None;
        self.drag = None;
        self.file_len = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragKind {
    /// A handle on the canvas.
    Handle(Hit),
    /// A time selection from the anchor.
    Select,
    /// The gain field, from this many dB.
    Gain(f32),
    /// The length field, from this length.
    Length(Tick),
}

struct Drag {
    kind: DragKind,
    /// The clip when the drag began; `None` while only selecting time.
    snapshot: Option<Snapshot>,
    /// The pointer at mouse-down.
    from: Point<Pixels>,
    /// The pointer's offset from the handle at mouse-down, so nothing jumps.
    grab: (f32, f32),
    anchor: Tick,
    moved: bool,
    /// Whether the project changed (Escape then undoes it).
    applied: bool,
    readout: Option<String>,
}

/// The clip and its file as the handlers and the painter see them this frame.
struct Frame {
    id: ClipId,
    clip: Clip,
    source: AudioSource,
    base: TimeBase,
    /// Length of the file in source ticks; 0 when unknown.
    source_len: Tick,
    /// End of the content on the axis.
    end: Tick,
    place: Option<Placed>,
    /// Axis position of song tick 0 (the grid is the song's).
    origin: Tick,
    sig: TimeSignature,
    bpm: f64,
    grid: GridSpec,
    layout: Layout,
}

impl Frame {
    fn of(ws: &Workspace) -> Option<Self> {
        let id = ws.s.selected_clip?;
        let clip = ws.s.project.clips.get(&id)?.clone();
        let Source::Audio(source) = ws.s.project.sources.get(&clip.source)? else { return None };
        let source = source.clone();
        let bpm = ws.s.project.tempo.initial_bpm();
        let base = TimeBase::of(&source, bpm);
        let source_len = match source.length_ticks(bpm) {
            0 => ws.sample.file_len.filter(|_| ws.sample.clip == Some(id)).unwrap_or(0),
            len => len,
        };
        let end = geom::content_end(&clip, source_len);
        let place = edit::placement_of(&ws.s, id);
        let origin = place.map_or(0, |pl| clip.offset - pl.start);
        let b = ws.sample.region.get();
        let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
        let axis = ws.sample.axis.filter(|_| ws.sample.clip == Some(id)).unwrap_or_else(|| Axis::fit(if w > 0. { w } else { 800. }, end));
        let sig = ws.s.project.tempo.signature_at(0);
        let grid = grid::adaptive(axis.px_per_beat as f32, sig, 0, false);
        Some(Self { id, clip, source, base, source_len, end, place, origin, sig, bpm, grid, layout: Layout { axis, w, h } })
    }

    /// Snap an axis position (`free` = Alt or Shift: no grid). The ends of
    /// the file and of the loop pull too; `loop_pulls` is off while the loop
    /// itself is dragged, or it would stick to where it is.
    fn snap(&self, u: f64, free: bool, loop_pulls: bool) -> Tick {
        let mut magnets = [0, self.source_len, 0, 0];
        let mut n = if self.source_len > 0 { 2 } else { 1 };
        if let Some(l) = self.clip.looping.filter(|_| loop_pulls) {
            magnets[n] = l.start;
            magnets[n + 1] = l.end;
            n += 2;
        }
        geom::snap(u, &self.grid, self.origin, self.layout.axis.px_per_tick(), &magnets[..n], free)
    }

    /// Zoom limits in px per beat: twice the whole file, down to single samples.
    fn zoom_limits(&self, max_zoom: f64) -> (f64, f64) {
        let fit = Axis::fit(self.layout.w.max(64.), self.end).px_per_beat;
        (fit * 0.5, max_zoom.max(fit))
    }

    /// Timeline seconds of a length in ticks.
    fn seconds(&self, ticks: Tick) -> String {
        geom::format_seconds(ticks as f64 * 60. / (self.bpm * PPQ as f64))
    }
}

/// Where the transport is on this clip's axis, and whether it is running.
fn playhead(ws: &Workspace, id: ClipId, clip: &Clip) -> Option<(Tick, bool)> {
    let t = ws.b.engine.transport();
    for track in &ws.s.project.tracks {
        for pl in track.arrangement.iter().filter(|pl| pl.clip == id) {
            if t.position >= pl.start && t.position < pl.start + clip.length {
                return Some((clip.offset + t.position - pl.start, t.playing));
            }
        }
    }
    if t.playing
        && let (Some(track), Some(scene)) = (ws.s.clip_track(id), ws.s.clip_scene(id))
        && let Some(SlotState::Playing { scene: playing, since }) = ws.b.engine.slot_state(track)
        && playing == scene
    {
        return Some((clip.offset + (t.position - since).rem_euclid(clip.length.max(1)), true));
    }
    None
}

fn audio_clip(ws: &Workspace) -> Option<(ClipId, &Clip)> {
    let id = ws.s.selected_clip?;
    let clip = ws.s.project.clips.get(&id)?;
    matches!(ws.s.project.sources.get(&clip.source), Some(Source::Audio(_))).then_some((id, clip))
}

/// Whether the editor needs continuous frames: one of its drags, or the
/// playhead running through the clip while the editor is on screen.
pub fn wants_frames(ws: &Workspace) -> bool {
    if ws.sample.drag.is_some() {
        return true;
    }
    if !ws.bottom_open || ws.s.bottom_tab != BottomTab::PianoRoll || !ws.b.engine.transport().playing {
        return false;
    }
    audio_clip(ws).is_some_and(|(id, clip)| playhead(ws, id, clip).is_some())
}

// ───────────────────────────── Public edits ─────────────────────────────

/// Split the clip at the editor's cursor (Ctrl+E while the editor has the
/// keys). The left part stays selected. Says why when it cannot.
pub fn split_at_cursor(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    let Some((id, _)) = audio_clip(ws) else { return false };
    let Some(cursor) = ws.sample.cursor.filter(|_| ws.sample.clip == Some(id)) else {
        ws.toast("Click the waveform to put the cursor where the clip should be split", cx);
        return false;
    };
    match edit::split(&mut ws.s, id, cursor) {
        Ok(()) => {
            ws.sample.selection = None;
            cx.notify();
            true
        }
        Err(why) => {
            ws.toast(why, cx);
            false
        }
    }
}

/// Keep only the selected time of the clip.
pub fn crop_to_selection(ws: &mut Workspace, cx: &mut Context<Workspace>) -> bool {
    let Some((id, _)) = audio_clip(ws) else { return false };
    let Some((a, b)) = ws.sample.selection.filter(|_| ws.sample.clip == Some(id)) else {
        ws.toast("Drag over the waveform to select what to keep", cx);
        return false;
    };
    let done = edit::crop(&mut ws.s, id, a, b);
    if done {
        ws.sample.selection = None;
        ws.sample.cursor = None;
        ws.sample.picked = None;
        cx.notify();
    } else {
        ws.toast("The selection has to cover part of the clip", cx);
    }
    done
}

/// Switch the clip's loop on (over what it plays now) or off.
pub fn toggle_loop(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    if let Some((id, _)) = audio_clip(ws)
        && edit::toggle_loop(&mut ws.s, id)
    {
        cx.notify();
    }
}

/// Show the whole file.
pub fn fit(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    ws.sample.axis = None;
    cx.notify();
}

fn zoom(ws: &mut Workspace, factor: f64, cx: &mut Context<Workspace>) {
    let Some(f) = Frame::of(ws) else { return };
    let (min, max) = f.zoom_limits(ws.sample.max_zoom);
    ws.sample.axis = Some(f.layout.axis.zoomed(f.layout.w / 2., factor, min, max).clamped(f.layout.w, f.end));
    cx.notify();
}

fn audition(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(f) = Frame::of(ws) else { return };
    if ws.sample.audition.take().is_some_and(|until| Instant::now() < until) {
        ws.b.engine.preview(None);
        cx.notify();
        return;
    }
    ws.b.engine.preview(Some(&f.source.path));
    let seconds = if f.source.frames > 0 { f.source.frames as f64 / f64::from(f.source.sample_rate.max(1)) } else { 4. };
    let length = Duration::from_secs_f64(seconds.clamp(0.2, 600.));
    ws.sample.audition = Some(Instant::now() + length);
    // The button goes back to rest when the file has played through.
    let rest = cx.spawn(async move |this, cx| {
        cx.background_executor().timer(length + Duration::from_millis(50)).await;
        let _ = this.update(cx, |this, cx| {
            if this.sample.audition.is_some_and(|until| Instant::now() >= until) {
                this.sample.audition = None;
                cx.notify();
            }
        });
    });
    rest.detach();
    cx.notify();
}

// ───────────────────────────── Drags ─────────────────────────────

fn local(ws: &Workspace, pos: Point<Pixels>) -> (f32, f32) {
    let b = ws.sample.region.get();
    (f32::from(pos.x - b.origin.x), f32::from(pos.y - b.origin.y))
}

/// Start a drag of the clip. `home` is where the dragged handle is now, in
/// canvas pixels; the pointer keeps its offset from it.
fn begin(ws: &mut Workspace, id: ClipId, kind: DragKind, home: (f32, f32), e: &MouseDownEvent, cx: &mut Context<Workspace>) {
    ws.s.seal();
    let (x, y) = local(ws, e.position);
    let snapshot = Snapshot::take(&ws.s, id);
    ws.sample.drag = Some(Drag { kind, snapshot, from: e.position, grab: (x - home.0, y - home.1), anchor: 0, moved: false, applied: false, readout: None });
    cx.notify();
}

fn canvas_down(ws: &mut Workspace, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Workspace>) {
    cx.stop_propagation();
    focus_handle(ws, cx).focus(window, cx);
    if e.first_mouse {
        return;
    }
    let Some(f) = Frame::of(ws) else { return };
    let (x, y) = local(ws, e.position);
    let (l, c) = (&f.layout, &f.clip);
    let hit = geom::hit_test(l, c, x, y);
    let (alt, double) = (e.modifiers.alt, e.click_count >= 2);
    let handle = |ws: &mut Workspace, hit: Hit, home: (f32, f32), cx: &mut Context<Workspace>| begin(ws, f.id, DragKind::Handle(hit), home, e, cx);
    ws.sample.picked = None;
    match hit {
        Hit::Point(i) => {
            if double {
                // Double-click a breakpoint to delete it (as in the automation lanes).
                edit::delete_point(&mut ws.s, f.id, i);
                ws.sample.hover = None;
            } else if let Some(home) = geom::envelope(c).get(i).and_then(|pt| geom::point_pos(l, c, pt)) {
                ws.sample.picked = Some(i);
                handle(ws, hit, home, cx);
            }
        }
        Hit::FadeIn | Hit::FadeOut => {
            let fade_in = hit == Hit::FadeIn;
            let fade = if fade_in { c.fade_in } else { c.fade_out };
            if double {
                // Double-click a fade handle to remove the fade.
                edit::apply_now(&mut ws.s, f.id, if fade_in { Gesture::FadeIn(c.offset) } else { Gesture::FadeOut(geom::end_u(c)) });
            } else if alt {
                // Alt + drag bends the fade instead of resizing it.
                let curve = if fade_in { Hit::FadeInCurve } else { Hit::FadeOutCurve };
                handle(ws, curve, (x, l.gain_y(curve_shape(0.5, fade.curve))), cx);
            } else {
                let edge = if fade_in { c.offset + fade.length } else { geom::end_u(c) - fade.length };
                handle(ws, hit, (l.x(edge), y), cx);
            }
        }
        Hit::FadeInCurve | Hit::FadeOutCurve => {
            let fade_in = hit == Hit::FadeInCurve;
            if double {
                // Double-click the slope to straighten it.
                edit::apply_now(&mut ws.s, f.id, if fade_in { Gesture::FadeInCurve(0.5) } else { Gesture::FadeOutCurve(0.5) });
            } else if let Some(home) = geom::fade_mid(l, c, fade_in) {
                handle(ws, hit, home, cx);
            }
        }
        Hit::Start => handle(ws, hit, (l.x(c.offset), y), cx),
        Hit::End => handle(ws, hit, (l.x(geom::end_u(c)), y), cx),
        Hit::LoopStart | Hit::LoopEnd | Hit::LoopBody => {
            if let Some(lp) = c.looping {
                handle(ws, hit, (l.x(if hit == Hit::LoopEnd { lp.end } else { lp.start }), y), cx);
            }
        }
        Hit::Segment(i) if alt => {
            // Alt + drag on the envelope line bends that segment.
            let points = geom::envelope(c);
            let middle = (points[i].tick + points[i + 1].tick) / 2;
            let value = c.volume_envelope().and_then(|lane| lane.value_at(middle)).unwrap_or(1.);
            handle(ws, hit, (x, l.gain_y(value)), cx);
        }
        Hit::Segment(_) | Hit::Body | Hit::Ruler => {
            let u = f.snap(l.axis.tick(x), alt, true);
            let inside = y >= RULER && u >= c.offset && u <= geom::end_u(c);
            if inside && (double || ws.sample.pencil) {
                // A new breakpoint under the pointer, dragged on in the same press.
                ws.s.seal();
                if let Some(index) = edit::add_point(&mut ws.s, f.id, u, l.gain_at_y(y)) {
                    let snapshot = Snapshot::take(&ws.s, f.id);
                    let grab = (x - l.x(u), 0.);
                    ws.sample.picked = Some(index);
                    ws.sample.drag =
                        Some(Drag { kind: DragKind::Handle(Hit::Point(index)), snapshot, from: e.position, grab, anchor: 0, moved: false, applied: true, readout: None });
                }
            } else {
                ws.sample.drag = Some(Drag { kind: DragKind::Select, snapshot: None, from: e.position, grab: (0., 0.), anchor: u, moved: false, applied: false, readout: None });
            }
        }
    }
    cx.notify();
}

fn canvas_right_down(ws: &mut Workspace, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    let Some(f) = Frame::of(ws) else { return };
    let (x, y) = local(ws, e.position);
    // Right-click a breakpoint to delete it.
    if let Hit::Point(i) = geom::hit_test(&f.layout, &f.clip, x, y) {
        cx.stop_propagation();
        edit::delete_point(&mut ws.s, f.id, i);
        ws.sample.hover = None;
        ws.sample.picked = None;
        cx.notify();
    }
}

fn canvas_hover(ws: &mut Workspace, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    if ws.sample.drag.is_some() {
        return;
    }
    let Some(f) = Frame::of(ws) else { return };
    let (x, y) = local(ws, e.position);
    let hit = match geom::hit_test(&f.layout, &f.clip, x, y) {
        // The line is only a target while Alt would bend it.
        Hit::Segment(_) if !e.modifiers.alt => Hit::Body,
        hit => hit,
    };
    if ws.sample.hover != Some(hit) {
        ws.sample.hover = Some(hit);
        cx.notify();
    }
}

fn drag_move(ws: &mut Workspace, e: &MouseMoveEvent, cx: &mut Context<Workspace>) {
    let Some(mut drag) = ws.sample.drag.take() else { return };
    let Some(f) = Frame::of(ws) else {
        // The clip went away under the drag.
        ws.s.seal();
        cx.notify();
        return;
    };
    let (dx, dy) = (f32::from(e.position.x - drag.from.x), f32::from(e.position.y - drag.from.y));
    if !drag.moved && dx.hypot(dy) < SLOP {
        ws.sample.drag = Some(drag);
        return;
    }
    drag.moved = true;
    // Shift moves a fifth as far.
    let fine = if e.modifiers.shift && drag.kind != DragKind::Select { 0.2 } else { 1. };
    let (from_x, from_y) = local(ws, drag.from);
    let (x, y) = (from_x + dx * fine - drag.grab.0, from_y + dy * fine - drag.grab.1);
    let l = &f.layout;
    let on_loop = matches!(drag.kind, DragKind::Handle(Hit::LoopStart | Hit::LoopEnd | Hit::LoopBody));
    let u = f.snap(l.axis.tick(x), e.modifiers.alt || fine < 1., !on_loop);
    let value = l.gain_at_y(y);
    let gesture = match drag.kind {
        DragKind::Handle(hit) => match hit {
            Hit::Start => Some(Gesture::Start(u)),
            Hit::End => Some(Gesture::End(u)),
            Hit::FadeIn => Some(Gesture::FadeIn(u)),
            Hit::FadeOut => Some(Gesture::FadeOut(u)),
            Hit::FadeInCurve => Some(Gesture::FadeInCurve(value)),
            Hit::FadeOutCurve => Some(Gesture::FadeOutCurve(value)),
            Hit::Point(index) => Some(Gesture::Point { index, u, value }),
            Hit::Segment(index) => Some(Gesture::Bend { index, mid: value }),
            Hit::LoopStart => Some(Gesture::LoopStart(u)),
            Hit::LoopEnd => Some(Gesture::LoopEnd(u)),
            Hit::LoopBody => Some(Gesture::LoopMove(u)),
            Hit::Ruler | Hit::Body => None,
        },
        DragKind::Select => {
            let a = drag.anchor;
            ws.sample.selection = (u != a).then_some((a.min(u), a.max(u)));
            ws.sample.cursor = Some(a);
            None
        }
        // 0.15 dB per pixel, in tenths.
        DragKind::Gain(start) => Some(Gesture::Gain(((start - dy * fine * 0.15) * 10.).round() / 10.)),
        // A beat every 8 px; sixteenths with Shift.
        DragKind::Length(start) => {
            let step = if e.modifiers.shift { PPQ / 4 } else { PPQ };
            let whole = (start + step / 2).div_euclid(step) * step;
            Some(Gesture::Length((whole + (-dy / 8.).round() as Tick * step).max(MIN_CLIP_LEN)))
        }
    };
    if let (Some(g), Some(snapshot)) = (gesture, &drag.snapshot) {
        drag.applied |= edit::apply(&mut ws.s, snapshot, g);
    }
    drag.readout = readout(ws, &f, drag.kind);
    ws.sample.drag = Some(drag);
    cx.notify();
}

/// What the dragged handle now reads, from the clip as it is.
fn readout(ws: &Workspace, f: &Frame, kind: DragKind) -> Option<String> {
    let DragKind::Handle(hit) = kind else { return None };
    let c = ws.s.project.clips.get(&f.id)?;
    Some(match hit {
        Hit::Start | Hit::End => format!("{}  ·  {}", geom::format_length(c.length, f.sig), f.seconds(c.length)),
        Hit::FadeIn => format!("Fade in {}", f.seconds(c.fade_in.length)),
        Hit::FadeOut => format!("Fade out {}", f.seconds(c.fade_out.length)),
        Hit::FadeInCurve => format!("Curve {:+.2}", c.fade_in.curve),
        Hit::FadeOutCurve => format!("Curve {:+.2}", c.fade_out.curve),
        Hit::Point(i) => geom::format_gain(geom::envelope(c).get(i)?.value),
        Hit::Segment(i) => format!("Curve {:+.2}", geom::envelope(c).get(i)?.curve),
        Hit::LoopStart | Hit::LoopEnd | Hit::LoopBody => {
            let lp = c.looping?;
            format!("Loop {}", geom::format_length(lp.end - lp.start, f.sig))
        }
        Hit::Ruler | Hit::Body => return None,
    })
}

fn drag_end(ws: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(drag) = ws.sample.drag.take() else { return };
    if drag.kind == DragKind::Select && !drag.moved {
        // A plain click: the edit cursor, and the song follows it when stopped.
        ws.sample.cursor = Some(drag.anchor);
        ws.sample.selection = None;
        if let Some(f) = Frame::of(ws)
            && let Some(pl) = f.place
            && !ws.b.engine.transport().playing
            && drag.anchor >= f.clip.offset
            && drag.anchor < geom::end_u(&f.clip)
        {
            ws.b.engine.locate(pl.start + drag.anchor - f.clip.offset);
        }
    }
    ws.s.seal();
    cx.notify();
}

/// Escape: give up the drag and put the clip back as it was.
fn cancel_drag(ws: &mut Workspace) -> bool {
    let Some(drag) = ws.sample.drag.take() else { return false };
    ws.s.seal();
    if drag.applied {
        ws.s.undo();
    }
    true
}

/// Window-wide mouse capture while one of the editor's own drags is active.
pub fn capture(ws: &Workspace, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    ws.sample.drag.as_ref()?;
    let weak = cx.entity().downgrade();
    Some(
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let w = weak.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, |this, cx| drag_move(this, e, cx));
                    }
                });
                let w = weak.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = w.update(cx, drag_end);
                    }
                });
            },
        )
        .absolute()
        .size_0()
        .into_any_element(),
    )
}

fn on_wheel(ws: &mut Workspace, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    let Some(f) = Frame::of(ws) else { return };
    let d = match e.delta {
        ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(l) => (l.x * 40., l.y * 40.),
    };
    let (x, _) = local(ws, e.position);
    let axis = if e.modifiers.secondary() || e.modifiers.control {
        // Around the pointer; a notch is about a third closer.
        let (min, max) = f.zoom_limits(ws.sample.max_zoom);
        f.layout.axis.zoomed(x, 2f64.powf(f64::from(d.1) / 100.), min, max)
    } else {
        f.layout.axis.scrolled(-(if d.0.abs() > d.1.abs() { d.0 } else { d.1 }))
    };
    ws.sample.axis = Some(axis.clamped(f.layout.w, f.end));
    cx.stop_propagation();
    cx.notify();
}

fn focus_handle(ws: &mut Workspace, cx: &mut Context<Workspace>) -> FocusHandle {
    ws.sample.focus.get_or_insert_with(|| cx.focus_handle()).clone()
}

/// The pointer over a target, or holding it.
fn cursor_for(hit: Option<Hit>, dragging: bool, pencil: bool) -> CursorStyle {
    match hit {
        Some(Hit::Start | Hit::End | Hit::LoopStart | Hit::LoopEnd | Hit::FadeIn | Hit::FadeOut) => CursorStyle::ResizeLeftRight,
        Some(Hit::FadeInCurve | Hit::FadeOutCurve | Hit::Segment(_)) => CursorStyle::ResizeUpDown,
        Some(Hit::Point(_) | Hit::LoopBody) => {
            if dragging {
                CursorStyle::ClosedHand
            } else {
                CursorStyle::OpenHand
            }
        }
        Some(Hit::Ruler | Hit::Body) | None => {
            if pencil {
                CursorStyle::Crosshair
            } else {
                CursorStyle::IBeam
            }
        }
    }
}

// ───────────────────────────── View ─────────────────────────────

/// A value that is dragged up and down, with its caption.
fn number_field(id: &'static str, caption: &'static str, value: String) -> Stateful<gpui::Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(CONTROL_H - SP1))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .cursor_ns_resize()
        .hover(|s| s.border_color(border_strong()))
        .child(label(caption, TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
        .child(mono(value, TEXT_SM, pal.text))
}

/// A quiet text button of the toolbar.
fn tool_button(id: &'static str, text: &'static str, enabled: bool) -> Stateful<gpui::Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .h(px(CONTROL_H))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .hover(|s| s.bg(hover()))
        .active(|s| s.bg(pressed()))
        .when(!enabled, |d| d.opacity(0.4))
        .child(label(text, TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM))
}

/// The clip editor for an audio clip (bottom panel, "Clip" tab).
pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let empty = || crate::views::devices::empty("Select an audio clip (Session or Arrangement) to edit its sample.");
    let Some(id) = audio_clip(ws).map(|(id, _)| id) else { return empty() };
    if ws.sample.clip != Some(id) {
        ws.sample.open(id);
    }
    let Some(mut f) = Frame::of(ws) else { return empty() };

    // The waveform: ready, on its way, or not readable.
    let peaks = ws.waves.peaks(&f.source.path);
    let note = match ws.waves.status(&f.source.path) {
        Status::Ready => None,
        Status::Failed(why) => Some(format!("Cannot read {}: {why}", f.source.path.display())),
        Status::Idle | Status::Loading => Some("Reading the waveform…".to_owned()),
    };
    // The file's time at the rate it was decoded at (normally the one the project stores).
    let decoded = TimeBase { sample_rate: peaks.as_ref().map_or(f.base.sample_rate, |p| f64::from(p.sample_rate().max(1))), ..f.base };
    let frames_per_beat = decoded.tick_to_frame(PPQ as f64);
    ws.sample.max_zoom = frames_per_beat / peaks.as_ref().map_or(16., |p| if p.has_samples() { 1. / SAMPLE_PX } else { p.finest() });
    let file_len = peaks.as_ref().map(|p| decoded.frame_to_tick(p.frames() as f64).round() as Tick);
    if f.source_len == 0 && ws.sample.file_len != file_len {
        // The project did not know how long the file is; now it is decoded.
        ws.sample.file_len = file_len;
        if let Some(known) = Frame::of(ws) {
            f = known;
        }
    }

    // Keep the view on the content (the clip may have changed length, the panel its width).
    if f.layout.w > 0. {
        f.layout.axis = f.layout.axis.clamped(f.layout.w, f.end);
        ws.sample.axis = Some(f.layout.axis);
    } else {
        // First frame: the canvas has no size yet.
        window.request_animation_frame();
    }

    let c = &f.clip;
    let looping = c.looping.is_some();
    let dragging = ws.sample.drag.is_some();
    let hot = match ws.sample.drag.as_ref().map(|d| d.kind) {
        Some(DragKind::Handle(hit)) => Some(hit),
        Some(_) => None,
        None => ws.sample.hover,
    };
    let scene = draw::Scene {
        clip: c.clone(),
        axis: f.layout.axis,
        base: decoded,
        origin: f.origin,
        grid: f.grid,
        peaks,
        note,
        cursor: ws.sample.cursor,
        selection: ws.sample.selection,
        playhead: playhead(ws, id, c),
        hot,
        picked: ws.sample.picked,
        readout: ws.sample.drag.as_ref().and_then(|d| d.readout.clone()),
        color: track_color(c.color),
    };
    let region = ws.sample.region.clone();
    let surface = canvas(move |b, _, _| region.set(b), move |b, _, window, cx| draw::paint(&scene, b, window, cx)).size_full();

    // ── Toolbar ──
    let auditioning = ws.sample.audition.is_some_and(|until| Instant::now() < until);
    let has_shaping = c.fade_in.length > 0 || c.fade_out.length > 0 || c.volume_envelope().is_some();
    let can_split = f.place.is_some() && ws.sample.cursor.is_some_and(|u| u > c.offset && u < geom::end_u(c));
    let can_crop = ws.sample.selection.is_some();
    let gain_db = c.gain_db;
    let length = c.length;
    let mut info = div().flex().items_center().gap(px(SP2));
    if f.place.is_none() {
        info = info.child(label("Not in the arrangement", TEXT_SM, pal.faint));
    }
    if let Some(lp) = c.looping.filter(|lp| lp.end > lp.start) {
        let passes = c.length as f64 / (lp.end - lp.start) as f64;
        if passes > 1.005 {
            let times = if (passes - passes.round()).abs() < 0.005 { format!("{passes:.0}") } else { format!("{passes:.2}") };
            info = info.child(label(format!("plays {times}× in the arrangement"), TEXT_SM, pal.muted));
        }
    }
    if let Some(key) = f.source.key {
        info = info.child(widgets::badge(key.display(), pal.accent));
    }
    if let Some(bpm) = f.source.warp.original_bpm {
        info = info.child(widgets::badge(format!("{bpm:.0} BPM"), pal.accent2));
    }
    if let Some(u) = ws.sample.cursor {
        // Where the cursor is in the file: time, and the sample itself once zoomed that far in.
        let at = geom::unroll(c.looping, u as f64).max(0.);
        let frame = if f.layout.axis.px_per_tick() * decoded.frame_to_tick(1.) >= 1. { format!("  #{:.0}", decoded.tick_to_frame(at)) } else { String::new() };
        info = info.child(mono(format!("{}{frame}", geom::format_seconds(decoded.seconds(at))), TEXT_SM, pal.text));
    }
    let channels = match f.source.channels {
        1 => "mono",
        2 => "stereo",
        _ => "multichannel",
    };
    info = info
        .child(mono(format!("{:.1} kHz {channels}", f64::from(f.source.sample_rate) / 1000.), TEXT_SM, pal.muted))
        .child(widgets::badge(f.grid.label(), pal.muted));

    let toolbar = div()
        .flex_none()
        .h(px(CONTROL_H + SP2))
        .px(px(SP2))
        .gap(px(SP1))
        .flex()
        .items_center()
        .border_b_1()
        .border_color(border())
        .child(
            widgets::icon_button("se-audition", Icon::Headphones, auditioning)
                .tooltip(widgets::tip("Audition the file", None))
                .on_click(cx.listener(|this, _, _, cx| audition(this, cx))),
        )
        .child(widgets::divider_v(16.))
        .child(
            widgets::icon_button("se-pencil", Icon::Pencil, ws.sample.pencil)
                .tooltip(widgets::tip("Draw volume breakpoints with a click (double-click works without it)", Some("B")))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.sample.pencil = !this.sample.pencil;
                    cx.notify();
                })),
        )
        .child(
            widgets::icon_button("se-loop", Icon::Loop, looping)
                .tooltip(widgets::tip("Loop the clip: drag the brace on the ruler, pull the end marker out to repeat", None))
                .on_click(cx.listener(|this, _, _, cx| toggle_loop(this, cx))),
        )
        .child(widgets::divider_v(16.))
        .child(
            widgets::icon_button("se-split", Icon::Scissors, false)
                .when(!can_split, |d| d.opacity(0.4))
                .tooltip(widgets::tip("Split at the cursor", Some("Ctrl+E")))
                .on_click(cx.listener(|this, _, _, cx| {
                    split_at_cursor(this, cx);
                })),
        )
        .child(tool_button("se-crop", "Crop", can_crop).tooltip(widgets::tip("Crop the clip to the selection", None)).on_click(cx.listener(|this, _, _, cx| {
            crop_to_selection(this, cx);
        })))
        .child(tool_button("se-reset", "Reset", has_shaping).tooltip(widgets::tip("Remove the fades and the volume envelope", None)).on_click(cx.listener(
            move |this, _, _, cx| {
                if edit::reset_shaping(&mut this.s, id) {
                    this.sample.picked = None;
                    cx.notify();
                }
            },
        )))
        .child(widgets::divider_v(16.))
        .child(
            number_field("se-gain", "GAIN", format!("{gain_db:+.1} dB"))
                .tooltip(widgets::tip("Clip gain: drag up or down, Shift for fine, double-click for 0 dB", None))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if e.click_count >= 2 {
                            this.sample.drag = None;
                            edit::apply_now(&mut this.s, id, Gesture::Gain(0.));
                            cx.notify();
                        } else {
                            begin(this, id, DragKind::Gain(gain_db), (0., 0.), e, cx);
                        }
                    }),
                ),
        )
        .child(
            number_field("se-length", "LENGTH", format!("{}  {}", geom::format_length(length, f.sig), f.seconds(length)))
                .tooltip(widgets::tip("Clip length in bars.beats.sixteenths: drag up or down, Shift for sixteenths", None))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        begin(this, id, DragKind::Length(length), (0., 0.), e, cx);
                    }),
                ),
        )
        .child(tool_button("se-half", "/2", length >= 2 * MIN_CLIP_LEN).tooltip(widgets::tip("Half as long", None)).on_click(cx.listener(move |this, _, _, cx| {
            if edit::apply_now(&mut this.s, id, Gesture::Length(length / 2)) {
                cx.notify();
            }
        })))
        .child(tool_button("se-double", "×2", true).tooltip(widgets::tip("Twice as long: past the end of the file the clip loops", None)).on_click(cx.listener(
            move |this, _, _, cx| {
                if edit::apply_now(&mut this.s, id, Gesture::Length(length * 2)) {
                    // Show what was added.
                    this.sample.axis = None;
                    cx.notify();
                }
            },
        )))
        .child(div().flex_1().min_w(px(SP2)))
        .child(div().flex_shrink(1.).min_w(px(0.)).overflow_hidden().child(info))
        .child(widgets::divider_v(16.))
        .child(widgets::icon_button("se-zoom-out", Icon::ZoomOut, false).tooltip(widgets::tip("Zoom out", Some("-"))).on_click(cx.listener(|this, _, _, cx| zoom(this, 1. / 1.5, cx))))
        .child(widgets::icon_button("se-zoom-in", Icon::ZoomIn, false).tooltip(widgets::tip("Zoom in", Some("+"))).on_click(cx.listener(|this, _, _, cx| zoom(this, 1.5, cx))))
        .child(tool_button("se-fit", "Fit", true).tooltip(widgets::tip("Show the whole file", Some("W"))).on_click(cx.listener(|this, _, _, cx| fit(this, cx))));

    let focus = focus_handle(ws, cx);
    div()
        .id("sample-editor")
        .size_full()
        .flex()
        .flex_col()
        .track_focus(&focus)
        // A click anywhere in the editor (toolbar included) gives it the keys.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                focus_handle(this, cx).focus(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &SplitClip, _, cx| {
            split_at_cursor(this, cx);
        }))
        .on_action(cx.listener(|this, _: &Escape, _, cx| {
            if cancel_drag(this) || this.sample.selection.take().is_some() {
                cx.notify();
            } else {
                cx.propagate();
            }
        }))
        .on_action(cx.listener(move |this, _: &DeleteSelection, _, cx| {
            // Delete removes the breakpoint clicked last; otherwise it is the workspace's.
            match this.sample.picked.take() {
                Some(index) if edit::delete_point(&mut this.s, id, index) => cx.notify(),
                _ => cx.propagate(),
            }
        }))
        .on_action(cx.listener(|this, _: &ToggleDrawTool, _, cx| {
            this.sample.pencil = !this.sample.pencil;
            cx.notify();
        }))
        .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
            if let Some(f) = Frame::of(this) {
                this.sample.selection = Some((f.clip.offset, geom::end_u(&f.clip)));
                cx.notify();
            }
        }))
        .on_action(cx.listener(|this, _: &ArrangeZoomIn, _, cx| zoom(this, 1.5, cx)))
        .on_action(cx.listener(|this, _: &ArrangeZoomOut, _, cx| zoom(this, 1. / 1.5, cx)))
        .on_action(cx.listener(|this, _: &ArrangeFit, _, cx| fit(this, cx)))
        .child(toolbar)
        .child(
            div()
                .id("sample-canvas")
                .flex_1()
                .min_h(px(0.))
                .w_full()
                .overflow_hidden()
                .cursor(cursor_for(hot, dragging, ws.sample.pencil))
                .child(surface)
                .on_mouse_down(MouseButton::Left, cx.listener(canvas_down))
                .on_mouse_down(MouseButton::Right, cx.listener(canvas_right_down))
                .on_mouse_move(cx.listener(canvas_hover))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if !*hovered && this.sample.drag.is_none() && this.sample.hover.take().is_some() {
                        cx.notify();
                    }
                }))
                .on_scroll_wheel(cx.listener(on_wheel)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_has_its_cursor() {
        // Resize cursors on the markers, the loop ends and the fades.
        for hit in [Hit::Start, Hit::End, Hit::LoopStart, Hit::LoopEnd, Hit::FadeIn, Hit::FadeOut] {
            assert_eq!(cursor_for(Some(hit), false, false), CursorStyle::ResizeLeftRight);
        }
        for hit in [Hit::FadeInCurve, Hit::FadeOutCurve, Hit::Segment(0)] {
            assert_eq!(cursor_for(Some(hit), false, false), CursorStyle::ResizeUpDown);
        }
        // A hand on the breakpoints, closed while one is held.
        assert_eq!(cursor_for(Some(Hit::Point(2)), false, false), CursorStyle::OpenHand);
        assert_eq!(cursor_for(Some(Hit::Point(2)), true, false), CursorStyle::ClosedHand);
        assert_eq!(cursor_for(Some(Hit::Body), false, false), CursorStyle::IBeam);
        assert_eq!(cursor_for(None, false, true), CursorStyle::Crosshair);
    }

    #[test]
    fn the_view_state_starts_over_for_another_clip() {
        let mut ui = UiState { cursor: Some(480), selection: Some((0, 960)), picked: Some(1), axis: Some(Axis { px_per_beat: 64., start: 0. }), ..UiState::default() };
        ui.pencil = true;
        ui.open(ClipId(7));
        assert_eq!((ui.clip, ui.cursor, ui.selection, ui.picked, ui.axis), (Some(ClipId(7)), None, None, None, None));
        assert!(ui.pencil, "the tool is the user's choice, not the clip's");
    }
}
