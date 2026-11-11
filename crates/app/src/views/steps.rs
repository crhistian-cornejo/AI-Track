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
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled,
    Window, canvas, div, prelude::FluentBuilder, px,
};

const HEAD_W: f32 = 176.;
const ROW: f32 = 28.;
const STEP: f32 = 28.;
const GAP: f32 = 4.;
const GROUP_GAP: f32 = 8.;

/// Left edge of step `i` relative to the grid.
fn step_x(i: usize) -> f32 {
    i as f32 * (STEP + GAP) + (i / 4) as f32 * (GROUP_GAP - GAP)
}

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some((track, clip)) = ws.s.step_clip() else {
        return crate::views::devices::empty("Add a Drum Rack to a track to use the step sequencer.");
    };
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
    let grid_w = step_x(steps - 1) + STEP;
    let grid_h = pads.len() as f32 * (ROW + GAP) - GAP;
    let grid = canvas(
        move |b, _, _| regions.steps.set(b),
        move |b, _, window, _| {
            let o = b.origin;
            for (r, (_, color, key)) in rows.iter().enumerate() {
                let y = r as f32 * (ROW + GAP);
                let c = track_color(*color);
                for i in 0..steps {
                    let x = step_x(i);
                    let start = i as Tick * step_ticks;
                    let hit = notes.iter().find(|(k, s, _)| k == key && *s >= start && *s < start + step_ticks);
                    let on_beat = (i / 4) % 2 == 0;
                    let bg = match hit {
                        Some((_, _, v)) => with_alpha(c, 0.55 + 0.45 * v),
                        None => content(if on_beat { 0.07 } else { 0.045 }),
                    };
                    window.paint_quad(paint::rrect(at(o, x, y, STEP, ROW), r_sm(), bg));
                    if playing_step == Some(i) {
                        window.paint_quad(paint::rrect_border(at(o, x, y, STEP, ROW), r_sm(), gpui::transparent_black(), accent(), 1.5));
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
        numbers = numbers.child(div().absolute().left(px(step_x(i))).child(mono(format!("{}", i / 4 + 1), TEXT_XS, pal.faint)));
    }
    let clip_name = ws.s.project.clips.get(&clip).map_or(String::new(), |c| c.name.clone());
    div()
        .id("steps-scroll")
        .size_full()
        .overflow_x_scroll()
        .overflow_y_scroll()
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
                                    let step = (0..steps).find(|i| x >= step_x(*i) && x < step_x(*i) + STEP);
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
