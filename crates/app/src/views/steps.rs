//! Step Sequencer (FL Studio channel rack, inside the drum rack): one row
//! per pad, 16 steps per bar. Clicking a step toggles a note through
//! `AppState::toggle_step`. The grid paints in one canvas.

use crate::bridge::SlotState;
use crate::paint::{self, at};
use crate::state::{BAR, drum_pads};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_model::*;
use crate::gesture::{self, Wheel};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, PinchEvent, ScrollHandle,
    ScrollWheelEvent, StatefulInteractiveElement, Styled, Window, canvas, div, point, prelude::FluentBuilder, px,
};

const HEAD_W: f32 = 176.;
const ROW: f32 = 28.;
const STEP: f32 = 28.;
const GAP: f32 = 4.;
const GROUP_GAP: f32 = 8.;

const MIN_ZOOM: f32 = 0.5;
const MAX_ZOOM: f32 = 2.5;

/// Left edge of step `i` relative to the grid, for steps `w` px wide.
fn step_x(i: usize, w: f32) -> f32 {
    i as f32 * (w + GAP) + (i / 4) as f32 * (GROUP_GAP - GAP)
}

/// The step under `x` px from the grid's left edge, as a fraction (2.5 = the
/// middle of the third step), so zooming can keep it under the pointer.
fn step_at(x: f32, w: f32) -> f32 {
    let group = 4. * (w + GAP) - GAP + GROUP_GAP;
    let g = (x / group).floor().max(0.);
    let rest = x - g * group;
    g * 4. + (rest / (w + GAP)).min(4.)
}

/// Where fractional step `f` is drawn (inverse of [`step_at`]).
fn step_pos(f: f32, w: f32) -> f32 {
    let i = f.floor().max(0.);
    step_x(i as usize, w) + (f - i) * (w + GAP)
}

/// Zoom and scroll of the step grid. Kept per window by GPUI (the
/// workspace has no field for it): the native scroll does two-finger pans
/// on both axes, Cmd/Ctrl + wheel and pinch change the step width.
struct View {
    zoom: f32,
    scroll: ScrollHandle,
}

/// Zoom by `factor` keeping the step under `pointer_x` (window px) still.
fn zoom_at(view: &mut View, grid_left: f32, pointer_x: f32, factor: f32) {
    let old = STEP * view.zoom;
    view.zoom = (view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let new = STEP * view.zoom;
    let x = pointer_x - grid_left;
    let moved = step_pos(step_at(x, old), new) - x;
    let off = view.scroll.offset();
    view.scroll.set_offset(point(px((f32::from(off.x) - moved).min(0.)), off.y));
}

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some((track, clip)) = ws.s.step_clip() else {
        return crate::views::devices::empty("Add a Drum Rack to a track to use the step sequencer.");
    };
    let view = window.use_keyed_state("step-view", cx, |_, _| View { zoom: 1., scroll: ScrollHandle::new() });
    let (zoom, scroll) = { let v = view.read(cx); (v.zoom, v.scroll.clone()) };
    let sw = STEP * zoom;
    let pads = ws.s.project.track(track).map(drum_pads).unwrap_or_default();
    let len = ws.s.clip_edit_len(clip);
    let bars = (len / BAR).max(1) as usize;
    let steps = bars * 16;
    let step_ticks = PPQ / 4;
    let notes: Vec<(u8, Tick, f32)> = ws.s.clip_notes(clip).map(|(_, n)| n.notes.iter().map(|n| (n.key, n.start, n.velocity)).collect()).unwrap_or_default();
    let t = ws.b.engine.transport();
    let playing_step = match ws.b.engine.slot_state(track) {
        Some(SlotState::Playing { since, .. }) if t.playing => Some((((t.position - since).rem_euclid(len)) / step_ticks) as usize),
        _ => None,
    };

    let mut heads = div().flex_none().w(px(HEAD_W)).flex().flex_col().gap(px(GAP));
    for (name, color, note) in &pads {
        heads = heads.child(
            div()
                .h(px(ROW))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .bg(pal.surface)
                .border_1()
                .border_color(border())
                .child(div().w(px(3.)).h(px(14.)).rounded(px(2.)).bg(track_color(*color)))
                .child(label(name.clone(), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM).flex_1())
                .child(mono(format!("{note}"), TEXT_XS, pal.faint)),
        );
    }
    let rows = pads.clone();
    let regions = ws.regions.clone();
    let grid_w = step_x(steps - 1, sw) + sw;
    let grid_h = pads.len() as f32 * (ROW + GAP) - GAP;
    let grid = canvas(
        move |b, _, _| regions.steps.set(b),
        move |b, _, window, _| {
            let o = b.origin;
            for (r, (_, color, key)) in rows.iter().enumerate() {
                let y = r as f32 * (ROW + GAP);
                let c = track_color(*color);
                for i in 0..steps {
                    let x = step_x(i, sw);
                    let start = i as Tick * step_ticks;
                    let hit = notes.iter().find(|(k, s, _)| k == key && *s >= start && *s < start + step_ticks);
                    let on_beat = (i / 4) % 2 == 0;
                    let bg = match hit {
                        Some((_, _, v)) => with_alpha(c, 0.55 + 0.45 * v),
                        None => content(if on_beat { 0.07 } else { 0.045 }),
                    };
                    window.paint_quad(paint::rrect(at(o, x, y, sw, ROW), r_sm(), bg));
                    if playing_step == Some(i) {
                        window.paint_quad(paint::rrect_border(at(o, x, y, sw, ROW), r_sm(), gpui::transparent_black(), accent(), 1.5));
                    }
                }
            }
        },
    )
    .w(px(grid_w))
    .h(px(grid_h));

    // Step numbers above the grid.
    let mut numbers = div().relative().h(px(16.)).w(px(grid_w));
    for i in (0..steps).step_by(4) {
        numbers = numbers.child(div().absolute().left(px(step_x(i, sw))).child(mono(format!("{}", i / 4 + 1), TEXT_XS, pal.faint)));
    }
    let clip_name = ws.s.project.clips.get(&clip).map_or(String::new(), |c| c.name.clone());
    let (wheel_view, pinch_view) = (view.clone(), view);
    div()
        .id("steps-scroll")
        .size_full()
        .overflow_x_scroll()
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .on_scroll_wheel(cx.listener(move |this, e: &ScrollWheelEvent, _, cx| {
            // Plain wheels and two-finger pans are the native scroll's.
            if let Wheel::Zoom { factor } = gesture::wheel(e) {
                let left = f32::from(this.regions.steps.get().origin.x);
                wheel_view.update(cx, |v, _| zoom_at(v, left, f32::from(e.position.x), factor));
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .on_pinch(cx.listener(move |this, e: &PinchEvent, _, cx| {
            let left = f32::from(this.regions.steps.get().origin.x);
            pinch_view.update(cx, |v, _| zoom_at(v, left, f32::from(e.position.x), gesture::pinch_factor(e)));
            cx.notify();
        }))
        .p(px(SP3))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(SP1))
                .child(
                    div()
                        .flex()
                        .gap(px(SP3))
                        .child(div().w(px(HEAD_W)).flex().items_center().gap(px(SP2)).child(label(clip_name, TEXT_SM, pal.muted)).child(mono(format!("{bars} bar · {steps} steps"), TEXT_XS, pal.faint)))
                        .child(numbers),
                )
                .child(
                    div().flex().gap(px(SP3)).child(heads).child(
                        div()
                            .id("step-grid")
                            .cursor_pointer()
                            .child(grid)
                            .when(true, |d| {
                                d.on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                    let b = this.regions.steps.get();
                                    let x = f32::from(e.position.x - b.origin.x);
                                    let y = f32::from(e.position.y - b.origin.y);
                                    let r = (y / (ROW + GAP)).floor() as usize;
                                    let step = (0..steps).find(|i| x >= step_x(*i, sw) && x < step_x(*i, sw) + sw);
                                    let pads = this.s.project.track(track).map(drum_pads).unwrap_or_default();
                                    if let (Some(i), Some((_, _, key))) = (step, pads.get(r)) {
                                        this.s.toggle_step(clip, *key, i as Tick * step_ticks, step_ticks);
                                        cx.notify();
                                    }
                                }))
                            }),
                    ),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_positions_round_trip_across_groups() {
        for w in [14., 28., 56.] {
            for f in [0., 0.5, 3.9, 4., 5.25, 15.5] {
                let back = step_at(step_pos(f, w), w);
                assert!((back - f).abs() < 1e-3, "{f} at {w}: {back}");
            }
            assert_eq!(step_pos(4., w), step_x(4, w));
        }
    }

    #[test]
    fn zoom_keeps_the_step_under_the_pointer() {
        let mut v = View { zoom: 1., scroll: ScrollHandle::new() };
        // The grid starts at 100 px; the pointer is over step 6.5.
        let pointer = 100. + step_pos(6.5, STEP);
        zoom_at(&mut v, 100., pointer, 2.);
        assert_eq!(v.zoom, 2.);
        let left = 100. + f32::from(v.scroll.offset().x);
        assert!((step_at(pointer - left, STEP * 2.) - 6.5).abs() < 1e-3);
        zoom_at(&mut v, left, pointer, 100.);
        assert_eq!(v.zoom, MAX_ZOOM);
    }
}
