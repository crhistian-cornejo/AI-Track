//! Piano Roll (FL Studio style): keyboard on the left, scale-highlighted
//! rows, ghost notes from the other clips in the scene, draw / select /
//! move / resize / delete, and a velocity lane. Everything paints in three
//! canvases; editing goes through `AppState` note commands.

use crate::bridge::SlotState;
use crate::paint::{self, at};
use crate::state::{BAR, Tool};
use crate::theme::*;
use crate::widgets::label;
use crate::workspace::{Drag, Workspace};
use aitrack_model::*;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    ScrollDelta, ScrollWheelEvent, Styled, TextAlign, Window, canvas, div, px,
};
use std::collections::BTreeSet;

const KEYS_W: f32 = 56.;
const RULER: f32 = 20.;
const VEL_H: f32 = 64.;

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(clip_id) = ws.s.selected_clip.filter(|c| ws.s.clip_notes(*c).is_some()) else {
        return crate::views::devices::empty("Select a MIDI clip (Session or Arrangement) to edit its notes.");
    };
    let clip = ws.s.project.clips.get(&clip_id).cloned().expect("selected clip exists");
    let notes: Vec<Note> = ws.s.clip_notes(clip_id).map(|(_, n)| n.notes.clone()).unwrap_or_default();
    let ghosts = ws.s.ghost_notes(clip_id);
    let scale = ws.s.clip_scale(Some(clip_id));
    let mask = scale.map_or(0xFFF, |s| s.mask());
    let root = scale.map(|s| s.root.0);
    let len = ws.s.clip_edit_len(clip_id);
    let view = ws.s.piano;
    let ppt = view.px_per_beat / PPQ as f32;
    let selected: BTreeSet<NoteId> = ws.s.selected_notes.clone();
    let color = clip.color;
    let snap_px = ws.s.snap.ticks() as f32 * ppt;
    let marquee = match &ws.drag {
        Some(Drag::Marquee { from, to, .. }) => Some((*from, *to)),
        _ => None,
    };

    // Where the clip is playing, if it is.
    let t = ws.b.engine.transport();
    let mut play_tick = None;
    if t.playing {
        if let (Some(track), Some(scene)) = (ws.s.clip_track(clip_id), ws.s.clip_scene(clip_id)) {
            if let Some(SlotState::Playing { scene: s, since }) = ws.b.engine.slot_state(track) {
                if s == scene {
                    play_tick = Some((t.position - since).rem_euclid(len));
                }
            }
        }
        if play_tick.is_none() {
            for tr in &ws.s.project.tracks {
                for pl in tr.arrangement.iter().filter(|pl| pl.clip == clip_id) {
                    if t.position >= pl.start && t.position < pl.start + clip.length {
                        play_tick = Some((t.position - pl.start).rem_euclid(len));
                    }
                }
            }
        }
    }

    // ── Ruler ──
    let ruler = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let o = b.origin;
            let w = f32::from(b.size.width);
            window.paint_quad(paint::fill_rect(b, p().surface));
            let bar_px = 4. * view.px_per_beat;
            let first = (view.scroll_x / bar_px).floor() as i64;
            for i in 0..=((w / bar_px).ceil() as i64 + 1) {
                let bar = first + i;
                let x = bar as f32 * bar_px - view.scroll_x;
                if bar < 0 {
                    continue;
                }
                window.paint_quad(paint::fill_rect(at(o, x, 8., 1., RULER - 8.), content(0.3)));
                paint::text(window, cx, &format!("{}", bar + 1), o.x + px(x + 4.), o.y + px(3.), TEXT_XS, p().muted, FontWeight::MEDIUM, TextAlign::Left);
                for beat in 1..4 {
                    window.paint_quad(paint::fill_rect(at(o, x + beat as f32 * view.px_per_beat, 14., 1., 6.), content(0.15)));
                }
            }
            let end = len as f32 * ppt - view.scroll_x;
            window.paint_quad(paint::fill_rect(at(o, end, 0., 2., RULER), accent_alpha(0.8)));
            window.paint_quad(paint::fill_rect(at(o, 0., RULER - 1., w, 1.), border()));
        },
    )
    .size_full();

    // ── Keyboard ──
    let keyboard = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let o = b.origin;
            let h = f32::from(b.size.height);
            let pal = p();
            window.paint_quad(paint::fill_rect(b, pal.surface));
            let first = (view.scroll_y / view.row_h).floor() as i32;
            let count = (h / view.row_h).ceil() as i32 + 1;
            for r in first..first + count {
                let key = 127 - r;
                if !(0..=127).contains(&key) {
                    continue;
                }
                let y = r as f32 * view.row_h - view.scroll_y;
                let pc = key % 12;
                let black = matches!(pc, 1 | 3 | 6 | 8 | 10);
                let in_scale = mask & (1 << pc) != 0;
                let fill = if black { if pal.dark { gpui::hsla(0., 0., 0.08, 1.) } else { gpui::hsla(0., 0., 0.22, 1.) } } else if pal.dark { gpui::hsla(0., 0., 0.78, 1.) } else { gpui::hsla(0., 0., 0.98, 1.) };
                let w = if black { KEYS_W * 0.62 } else { KEYS_W - 1. };
                window.paint_quad(paint::fill_rect(at(o, 0., y, w, view.row_h - 1.), fill));
                if in_scale && scale.is_some() {
                    window.paint_quad(paint::fill_rect(at(o, KEYS_W - 4., y + 1., 3., view.row_h - 3.), if Some(pc as u8) == root { accent() } else { accent_alpha(0.45) }));
                }
                if pc == 0 && view.row_h >= 9. {
                    let name = format!("C{}", key / 12 - 1);
                    paint::text(window, cx, &name, o.x + px(KEYS_W - 8.), o.y + px(y + (view.row_h - 12.) / 2.), TEXT_XS, gpui::hsla(0., 0., 0.25, 1.), FontWeight::MEDIUM, TextAlign::Right);
                }
            }
            window.paint_quad(paint::fill_rect(at(o, KEYS_W - 1., 0., 1., h), border()));
        },
    )
    .size_full();

    // ── Grid + notes ──
    let regions = ws.regions.clone();
    let grid_notes = notes.clone();
    let grid_sel = selected.clone();
    let grid = canvas(
        move |b, _, _| regions.piano.set(b),
        move |b, _, window, _| {
            let o = b.origin;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            let pal = p();
            window.paint_quad(paint::fill_rect(b, pal.row_out));
            let first = (view.scroll_y / view.row_h).floor() as i32;
            let count = (h / view.row_h).ceil() as i32 + 1;
            for r in first..first + count {
                let key = 127 - r;
                if !(0..=127).contains(&key) {
                    continue;
                }
                let y = r as f32 * view.row_h - view.scroll_y;
                let pc = key % 12;
                let in_scale = mask & (1 << pc) != 0;
                let bg = if Some(pc as u8) == root { theme_mix(pal.row_scale, pal.accent, 0.06) } else if in_scale { pal.row_scale } else { pal.row_out };
                window.paint_quad(paint::fill_rect(at(o, 0., y, w, view.row_h), bg));
                window.paint_quad(paint::fill_rect(at(o, 0., y + view.row_h - 1., w, 1.), content(if pc == 0 { 0.08 } else { 0.03 })));
            }
            // Vertical grid: snap lines, beats, bars.
            let first_tick = view.scroll_x / ppt;
            if snap_px >= 6. {
                let step = snap_px;
                let start = (view.scroll_x / step).floor() as i64;
                for i in 0..=((w / step).ceil() as i64 + 1) {
                    let x = (start + i) as f32 * step - view.scroll_x;
                    window.paint_quad(paint::fill_rect(at(o, x, 0., 1., h), content(0.025)));
                }
            }
            let beat_start = (first_tick / PPQ as f32).floor() as i64;
            for i in 0..=((w / view.px_per_beat).ceil() as i64 + 1) {
                let beat = beat_start + i;
                let x = beat as f32 * view.px_per_beat - view.scroll_x;
                let bar = beat % 4 == 0;
                window.paint_quad(paint::fill_rect(at(o, x, 0., 1., h), content(if bar { 0.11 } else { 0.05 })));
            }
            // Outside the clip.
            let end = len as f32 * ppt - view.scroll_x;
            if end < w {
                window.paint_quad(paint::fill_rect(at(o, end.max(0.), 0., w - end.max(0.), h), with_alpha(pal.base, 0.55)));
            }
            let note_rect = |n: &Note| {
                let x = n.start as f32 * ppt - view.scroll_x;
                let y = (127 - n.key as i32) as f32 * view.row_h - view.scroll_y;
                (x, y, (n.length as f32 * ppt).max(3.), view.row_h)
            };
            for (c, n) in &ghosts {
                let (x, y, nw, nh) = note_rect(n);
                if x > w || x + nw < 0. || y > h || y + nh < 0. {
                    continue;
                }
                window.paint_quad(paint::rrect(at(o, x + 1., y + 1., nw - 2., nh - 2.), 2., with_alpha(track_color(*c), 0.22)));
            }
            let fill = track_color(color);
            for n in &grid_notes {
                let (x, y, nw, nh) = note_rect(n);
                if x > w || x + nw < 0. || y > h || y + nh < 0. {
                    continue;
                }
                let sel = grid_sel.contains(&n.id);
                let body = with_alpha(if sel { theme_mix(fill, gpui::white(), 0.35) } else { fill }, 0.55 + 0.45 * n.velocity);
                window.paint_quad(paint::rrect_border(at(o, x + 0.5, y + 0.5, nw - 1., nh - 1.), 2.5, body, if sel { accent() } else { with_alpha(gpui::black(), 0.35) }, if sel { 1.5 } else { 1. }));
                // Resize grip.
                if nw > 10. {
                    window.paint_quad(paint::fill_rect(at(o, x + nw - 4., y + 3., 1., nh - 6.), with_alpha(gpui::black(), 0.3)));
                }
            }
            if let Some((a, bb)) = marquee {
                let x0 = f32::from(a.x.min(bb.x) - o.x);
                let y0 = f32::from(a.y.min(bb.y) - o.y);
                let x1 = f32::from(a.x.max(bb.x) - o.x);
                let y1 = f32::from(a.y.max(bb.y) - o.y);
                window.paint_quad(paint::rrect_border(at(o, x0, y0, x1 - x0, y1 - y0), 2., accent_alpha(0.1), accent_alpha(0.8), 1.));
            }
            if let Some(tick) = play_tick {
                let x = tick as f32 * ppt - view.scroll_x;
                window.paint_quad(paint::fill_rect(at(o, x, 0., 1., h), accent()));
            }
        },
    )
    .size_full();

    // ── Velocity lane ──
    let regions = ws.regions.clone();
    let vel_notes = notes;
    let vel_sel = selected;
    let velocity = canvas(
        move |b, _, _| regions.velocity.set(b),
        move |b, _, window, _| {
            let o = b.origin;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            window.paint_quad(paint::fill_rect(b, p().well));
            for frac in [0.25, 0.5, 0.75] {
                window.paint_quad(paint::fill_rect(at(o, 0., 4. + (h - 8.) * frac, w, 1.), content(0.04)));
            }
            let fill = track_color(color);
            for n in &vel_notes {
                let x = n.start as f32 * ppt - view.scroll_x;
                if x < -8. || x > w + 8. {
                    continue;
                }
                let sel = vel_sel.contains(&n.id);
                let c = if sel { accent() } else { fill };
                let top = 4. + (h - 8.) * (1. - n.velocity);
                window.paint_quad(paint::fill_rect(at(o, x, top, 2., h - 4. - top), with_alpha(c, 0.8)));
                window.paint_quad(paint::rrect(at(o, x - 2.5, top - 3., 7., 7.), 3.5, c));
            }
            window.paint_quad(paint::fill_rect(at(o, 0., 0., w, 1.), border()));
        },
    )
    .size_full();

    let name = clip.name.clone();
    let bars = len / BAR;
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_none()
                .h(px(RULER))
                .flex()
                .child(div().w(px(KEYS_W)).h_full().bg(pal.surface).border_r_1().border_b_1().border_color(border()))
                .child(div().flex_1().h_full().child(ruler)),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .child(div().w(px(KEYS_W)).h_full().child(keyboard))
                .child(
                    div()
                        .id("piano-grid")
                        .flex_1()
                        .h_full()
                        .cursor(if ws.s.tool == Tool::Draw { gpui::CursorStyle::Crosshair } else { gpui::CursorStyle::Arrow })
                        .child(grid)
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| grid_down(this, clip_id, e, false, cx)))
                        .on_mouse_down(MouseButton::Right, cx.listener(move |this, e: &MouseDownEvent, _, cx| grid_down(this, clip_id, e, true, cx)))
                        .on_scroll_wheel(cx.listener(on_wheel)),
                ),
        )
        .child(
            div()
                .flex_none()
                .h(px(VEL_H))
                .flex()
                .child(
                    div()
                        .w(px(KEYS_W))
                        .h_full()
                        .bg(pal.surface)
                        .border_r_1()
                        .border_t_1()
                        .border_color(border())
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .child(label("VEL", TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD))
                        .child(label(format!("{bars} bar{}", if bars == 1 { "" } else { "s" }), TEXT_XS, pal.faint))
                        .child(label(crate::paint::fit(&name, KEYS_W - 4., TEXT_XS), TEXT_XS, pal.faint)),
                )
                .child(
                    div()
                        .id("velocity-lane")
                        .flex_1()
                        .h_full()
                        .cursor_ns_resize()
                        .child(velocity)
                        .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| velocity_down(this, clip_id, e, cx)))
                        .on_scroll_wheel(cx.listener(on_wheel)),
                ),
        )
        .into_any_element()
}

fn theme_mix(a: gpui::Hsla, b: gpui::Hsla, t: f32) -> gpui::Hsla {
    crate::theme::mix(a, b, t)
}

fn note_at(ws: &Workspace, clip: ClipId, tick: Tick, key: i32) -> Option<Note> {
    let (_, n) = ws.s.clip_notes(clip)?;
    n.notes.iter().rev().find(|n| n.key as i32 == key && tick >= n.start && tick < n.end()).cloned()
}

fn grid_down(ws: &mut Workspace, clip: ClipId, e: &MouseDownEvent, right: bool, cx: &mut Context<Workspace>) {
    if e.first_mouse {
        return;
    }
    let (tick, key) = ws.piano_pos(e.position);
    let hit = note_at(ws, clip, tick, key);
    if right {
        if let Some(n) = hit {
            let ids = if ws.s.selected_notes.contains(&n.id) { ws.s.selected_notes.iter().copied().collect() } else { vec![n.id] };
            ws.s.delete_notes(clip, ids);
            cx.notify();
        }
        return;
    }
    match hit {
        Some(n) => {
            if e.click_count >= 2 {
                ws.s.delete_notes(clip, vec![n.id]);
                cx.notify();
                return;
            }
            if e.modifiers.shift {
                if !ws.s.selected_notes.remove(&n.id) {
                    ws.s.selected_notes.insert(n.id);
                }
                cx.notify();
                return;
            }
            if !ws.s.selected_notes.contains(&n.id) {
                ws.s.selected_notes = [n.id].into();
            }
            let ppt = ws.s.piano.px_per_beat / PPQ as f32;
            let resize = (n.end() - tick) as f32 * ppt < 6.;
            let originals = selected_notes(ws, clip);
            ws.begin_drag(Drag::Notes { clip, originals, origin: (tick, key), resize, applied: (0, 0) }, cx);
        }
        None if ws.s.tool == Tool::Draw && !e.modifiers.shift => {
            if let Some(id) = ws.s.draw_note(clip, key as u8, tick) {
                let originals = ws.s.clip_notes(clip).map(|(_, n)| n.notes.iter().filter(|n| n.id == id).cloned().collect()).unwrap_or_default();
                // Keep the drawn note under the mouse while dragging (FL-style).
                ws.drag = None;
                ws.drag = Some(Drag::Notes { clip, originals, origin: (tick, key), resize: false, applied: (0, 0) });
                cx.notify();
            }
        }
        None => {
            let base = if e.modifiers.shift { ws.s.selected_notes.clone() } else { BTreeSet::new() };
            ws.s.selected_notes = base.clone();
            ws.begin_drag(Drag::Marquee { clip, from: e.position, to: e.position, base }, cx);
        }
    }
}

fn selected_notes(ws: &Workspace, clip: ClipId) -> Vec<Note> {
    ws.s.clip_notes(clip).map(|(_, n)| n.notes.iter().filter(|n| ws.s.selected_notes.contains(&n.id)).cloned().collect()).unwrap_or_default()
}

fn velocity_down(ws: &mut Workspace, clip: ClipId, e: &MouseDownEvent, cx: &mut Context<Workspace>) {
    let b = ws.regions.velocity.get();
    let x = f32::from(e.position.x - b.origin.x) + ws.s.piano.scroll_x;
    let ppt = ws.s.piano.px_per_beat / PPQ as f32;
    let Some((_, src)) = ws.s.clip_notes(clip) else { return };
    let hit = src
        .notes
        .iter()
        .map(|n| ((n.start as f32 * ppt - x).abs(), n.clone()))
        .filter(|(d, _)| *d < 8.)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, n)| n);
    let Some(n) = hit else { return };
    let originals = if ws.s.selected_notes.contains(&n.id) { selected_notes(ws, clip) } else { vec![n] };
    let lane_h = f32::from(b.size.height) - 8.;
    ws.begin_drag(Drag::Velocity { clip, originals, start_y: f32::from(e.position.y), lane_h }, cx);
}

fn on_wheel(ws: &mut Workspace, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    let d = match e.delta {
        ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(l) => (l.x * 40., l.y * 40.),
    };
    let b = ws.regions.piano.get();
    let v = &mut ws.s.piano;
    if e.modifiers.secondary() || e.modifiers.control {
        let mx = f32::from(e.position.x - b.origin.x).max(0.);
        let beat_at = (mx + v.scroll_x) / v.px_per_beat;
        v.px_per_beat = (v.px_per_beat * (1. + d.1 / 400.)).clamp(12., 320.);
        v.scroll_x = (beat_at * v.px_per_beat - mx).max(0.);
    } else if e.modifiers.alt {
        v.row_h = (v.row_h + d.1.signum()).clamp(8., 24.);
    } else if e.modifiers.shift || d.0.abs() > d.1.abs() {
        let dx = if d.0.abs() > d.1.abs() { d.0 } else { d.1 };
        v.scroll_x = (v.scroll_x - dx).max(0.);
    } else {
        let max = (128. * v.row_h - f32::from(b.size.height)).max(0.);
        v.scroll_y = (v.scroll_y - d.1).clamp(0., max);
    }
    cx.notify();
}
