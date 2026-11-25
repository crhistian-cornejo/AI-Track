//! The Step Sequencer clip's editor (Live 13), shown in the Clip View instead
//! of the piano roll when the clip holds a step pattern.
//!
//! One row per key. Each row has its own length, rate, direction and swing in
//! the head at the left; the grid paints its steps in one canvas (click to
//! switch a step). Under the grid a lane edits one value of the selected
//! row's steps: velocity, chance or repeat (click or drag the bars). Every
//! change is `AppState::edit_steps*`, so it is a `Command` and undoes.

use crate::bridge::SlotState;
use crate::paint::{self, at};
use crate::state::drum_pads;
use crate::theme::*;
use crate::views::bottom::{key_name, small_button};
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_model::steps::{MAX_REPEAT, MAX_ROWS};
use aitrack_model::*;
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, StatefulInteractiveElement,
    Styled, Window, canvas, div, prelude::FluentBuilder, px,
};
use std::rc::Rc;

const HEAD_W: f32 = 316.;
const ROW: f32 = 28.;
const STEP: f32 = 28.;
const GAP: f32 = 4.;
const GROUP_GAP: f32 = 8.;
const LANE_H: f32 = 80.;

/// Left edge of step `i` relative to the grid.
fn step_x(i: usize) -> f32 {
    i as f32 * (STEP + GAP) + (i / 4) as f32 * (GROUP_GAP - GAP)
}

/// The step under `x` px from the grid's left edge.
fn step_at(x: f32, steps: usize) -> Option<usize> {
    (0..steps).find(|i| x >= step_x(*i) && x < step_x(*i) + STEP)
}

/// What the lane under the grid edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
    Velocity,
    Chance,
    Repeat,
}

impl Lane {
    const ALL: [Lane; 3] = [Lane::Velocity, Lane::Chance, Lane::Repeat];

    fn of(i: usize) -> Lane {
        Self::ALL[i.min(2)]
    }

    /// The step's value as a 0..=1 bar height.
    fn height(self, s: Step) -> f32 {
        match self {
            Lane::Velocity => s.velocity,
            Lane::Chance => s.chance,
            Lane::Repeat => f32::from(s.repeat) / f32::from(MAX_REPEAT),
        }
    }

    /// Write the bar height `h` (0..=1, from the pointer) into `s`.
    fn set(self, s: &mut Step, h: f32) {
        let h = h.clamp(0.0, 1.0);
        match self {
            Lane::Velocity => s.velocity = h.max(0.05),
            Lane::Chance => s.chance = (h * 20.0).round() / 20.0,
            Lane::Repeat => s.repeat = ((h * f32::from(MAX_REPEAT)).ceil() as u8).clamp(1, MAX_REPEAT),
        }
    }
}

/// The editor's own state, kept per window by GPUI.
struct View {
    row: usize,
    lane: Lane,
}

/// Where the song is inside the clip (ticks from its start), when it plays.
fn playing_at(ws: &Workspace, track: TrackId, clip: ClipId, span: Tick) -> Option<Tick> {
    let t = ws.b.engine.transport();
    if !t.playing {
        return None;
    }
    if let Some(SlotState::Playing { since, scene }) = ws.b.engine.slot_state(track)
        && ws.s.project.track(track).and_then(|tr| tr.session_slots.get(&scene)) == Some(&clip)
    {
        return Some((t.position - since).rem_euclid(span));
    }
    let tr = ws.s.project.track(track)?;
    let len = ws.s.project.clips.get(&clip)?.length;
    let pl = tr.arrangement.iter().find(|pl| pl.clip == clip && t.position >= pl.start && t.position < pl.start + len)?;
    Some((t.position - pl.start).rem_euclid(span))
}

pub fn render(ws: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(clip) = ws.s.selected_clip.filter(|c| ws.s.clip_pattern(*c).is_some()) else {
        return crate::views::devices::empty("Select a step sequencer clip to edit its steps.");
    };
    let Some(pattern) = ws.s.clip_pattern(clip).cloned() else { return div().into_any_element() };
    let track = ws.s.clip_track(clip);
    let (name, color) = ws.s.project.clips.get(&clip).map_or((String::new(), Color(0)), |c| (c.name.clone(), c.color));
    let pads = track.and_then(|t| ws.s.project.track(t)).map(drum_pads).unwrap_or_default();
    let view = window.use_keyed_state("step-clip-view", cx, |_, _| View { row: 0, lane: Lane::Velocity });
    let (sel_row, lane) = {
        let v = view.read(cx);
        (v.row.min(pattern.rows.len().saturating_sub(1)), v.lane)
    };
    let c = track_color(color);
    let max_len = pattern.rows.iter().map(|r| usize::from(r.length)).max().unwrap_or(16).max(1);
    let play = track.and_then(|t| playing_at(ws, t, clip, pattern.span));

    // ── Row heads ──
    let mut heads = div().flex_none().w(px(HEAD_W)).flex().flex_col().gap(px(GAP));
    for (r, row) in pattern.rows.iter().enumerate() {
        let n = clip.0 as usize * 64 + r;
        let selected = r == sel_row;
        let key = row.key;
        let title = pads.iter().find(|(_, _, k)| *k == key).map_or_else(|| key_name(key), |(name, _, _)| name.clone());
        let pick = view.clone();
        heads = heads.child(
            div()
                .h(px(ROW))
                .px(px(SP1))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .bg(if selected { accent_alpha(0.12) } else { pal.surface })
                .border_1()
                .border_color(if selected { accent_alpha(0.4) } else { border() })
                .child(
                    div()
                        .id(("step-row-pick", n))
                        .flex_1()
                        .min_w(px(0.))
                        .h_full()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .cursor_pointer()
                        .tooltip(widgets::tip("Select the row: the lane below edits its steps", None))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            pick.update(cx, |v, _| v.row = r);
                            cx.notify();
                        }))
                        .child(div().w(px(3.)).h(px(14.)).rounded(px(2.)).bg(c))
                        .child(label(crate::paint::fit(&title, 64., TEXT_SM), TEXT_SM, pal.text).font_weight(FontWeight::MEDIUM)),
                )
                .child(small_button(("step-len-down", n), "−", row.length > 1).tooltip(widgets::tip("Shorter row (fewer steps before it restarts)", None)).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.s.resize_step_row(clip, r, -1);
                        cx.notify();
                    },
                )))
                .child(div().w(px(24.)).flex().justify_center().child(mono(format!("{}", row.length), TEXT_SM, pal.text)))
                .child(small_button(("step-len-up", n), "+", row.length < steps::MAX_ROW_STEPS).tooltip(widgets::tip("Longer row", None)).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.s.resize_step_row(clip, r, 1);
                        cx.notify();
                    },
                )))
                .child(
                    small_button(("step-rate", n), row.rate.label(), true)
                        .tooltip(widgets::tip("Step rate: click for faster, right-click for slower", None))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.s.step_row_rate(clip, r, 1);
                            cx.notify();
                        }))
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| {
                                this.s.step_row_rate(clip, r, -1);
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
                .child(small_button(("step-dir", n), row.direction.label(), true).tooltip(widgets::tip("Direction: forward, backward, ping-pong, random", None)).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.s.cycle_step_row_direction(clip, r);
                        cx.notify();
                    },
                )))
                .child(small_button(("step-swing", n), format!("{:.0}%", row.swing * 100.), true).tooltip(widgets::tip("Swing: delays every second step", None)).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.s.cycle_step_row_swing(clip, r);
                        cx.notify();
                    },
                )))
                .child(small_button(("step-row-del", n), "×", pattern.rows.len() > 1).tooltip(widgets::tip("Delete the row", None)).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.s.remove_step_row(clip, r);
                        cx.notify();
                    },
                ))),
        );
    }

    // ── Grid ──
    let grid_w = step_x(max_len - 1) + STEP;
    let grid_h = pattern.rows.len() as f32 * (ROW + GAP) - GAP;
    let rows = pattern.rows.clone();
    let regions = ws.regions.clone();
    let step_ticks: Vec<Tick> = rows.iter().map(|r| r.rate.ticks()).collect();
    let grid = canvas(
        move |b, _, _| regions.step_grid.set(b),
        move |b, _, window, _| {
            let o = b.origin;
            for (r, row) in rows.iter().enumerate() {
                let y = r as f32 * (ROW + GAP);
                // The step the song is on in this row (its direction applied).
                let now = play.map(|t| row.step_index((t / step_ticks[r]) as u64));
                for i in 0..max_len {
                    let x = step_x(i);
                    let rect = at(o, x, y, STEP, ROW);
                    if i >= usize::from(row.length) {
                        window.paint_quad(paint::rrect(rect, r_sm(), content(0.02)));
                        continue;
                    }
                    let s = row.step(i);
                    let bg = if s.on { with_alpha(c, 0.45 + 0.55 * s.velocity) } else { content(if (i / 4) % 2 == 0 { 0.07 } else { 0.045 }) };
                    window.paint_quad(paint::rrect(rect, r_sm(), bg));
                    if s.on && s.chance < 1.0 {
                        // The chance is the width of the bar along the cell's foot.
                        window.paint_quad(paint::rrect(at(o, x + 2., y + ROW - 6., (STEP - 4.) * s.chance, 4.), 2., content(0.75)));
                    }
                    if s.on && s.repeat > 1 {
                        let w = (STEP - 4.) / f32::from(s.repeat);
                        for h in 1..s.repeat {
                            window.paint_quad(paint::rrect(at(o, x + 2. + w * f32::from(h) - 1., y + 4., 2., 8.), 1., content(0.75)));
                        }
                    }
                    if now == Some(i) {
                        window.paint_quad(paint::rrect_border(rect, r_sm(), gpui::transparent_black(), accent(), 1.5));
                    }
                }
            }
        },
    )
    .w(px(grid_w))
    .h(px(grid_h));

    // ── Lane ──
    let row_len = pattern.rows.get(sel_row).map_or(0, |r| usize::from(r.length));
    let sel_steps: Vec<Step> = pattern.rows.get(sel_row).map(|r| (0..row_len).map(|i| r.step(i)).collect()).unwrap_or_default();
    let regions = ws.regions.clone();
    let lane_canvas = canvas(
        move |b, _, _| regions.step_lane.set(b),
        move |b, _, window, _| {
            let o = b.origin;
            for i in 0..max_len {
                let x = step_x(i);
                window.paint_quad(paint::rrect(at(o, x, 0., STEP, LANE_H), r_sm(), content(if i < row_len { 0.05 } else { 0.02 })));
                if let Some(s) = sel_steps.get(i) {
                    let h = (lane.height(*s) * LANE_H).max(2.);
                    window.paint_quad(paint::rrect(at(o, x, LANE_H - h, STEP, h), r_sm(), with_alpha(c, if s.on { 0.9 } else { 0.18 })));
                }
            }
        },
    )
    .w(px(grid_w))
    .h(px(LANE_H));
    let edit_lane = move |this: &mut Workspace, x: f32, y: f32, cx: &mut Context<Workspace>| {
        let Some(i) = step_at(x, row_len) else { return };
        let h = 1. - y / LANE_H;
        this.s.edit_steps_coalesced(clip, &format!("step-lane-{}", clip.0), "Edit steps", |p| {
            if let Some(r) = p.rows.get_mut(sel_row)
                && r.step(i).on
            {
                r.edit_step(i, |s| lane.set(s, h));
            }
        });
        cx.notify();
    };
    let on_down = edit_lane;
    let on_move = edit_lane;

    let weak = cx.entity().downgrade();
    let lane_view = view.clone();
    let on_lane: widgets::OnSelect = Rc::new(move |i, _, cx| {
        let _ = weak.update(cx, |_, cx| {
            lane_view.update(cx, |v, _| v.lane = Lane::of(i));
            cx.notify();
        });
    });
    let lane_tabs = widgets::segmented("step-lane-tabs", &[(None, "Velocity"), (None, "Chance"), (None, "Repeat")], Lane::ALL.iter().position(|l| *l == lane).unwrap_or(0), 72., on_lane);

    let toolbar = div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(label(name, TEXT_SM, pal.muted))
        .child(mono(format!("{} rows · {} steps max", pattern.rows.len(), max_len), TEXT_XS, pal.faint))
        .child(div().flex_1())
        .child(small_button("step-add-row", "+ Row", pattern.rows.len() < MAX_ROWS).tooltip(widgets::tip("Add a row below, a key lower", None)).on_click(cx.listener(move |this, _, _, cx| {
            this.s.add_step_row(clip);
            cx.notify();
        })))
        .child(small_button("step-key-down", "Key −", !pattern.rows.is_empty()).tooltip(widgets::tip("Move the selected row down a semitone", None)).on_click(cx.listener(
            move |this, _, _, cx| {
                this.s.transpose_step_row(clip, sel_row, -1);
                cx.notify();
            },
        )))
        .child(small_button("step-key-up", "Key +", !pattern.rows.is_empty()).tooltip(widgets::tip("Move the selected row up a semitone", None)).on_click(cx.listener(
            move |this, _, _, cx| {
                this.s.transpose_step_row(clip, sel_row, 1);
                cx.notify();
            },
        )))
        .child(small_button("step-to-notes", "Convert to Notes", true).tooltip(widgets::tip("Turn the steps into plain MIDI notes you can edit freely", None)).on_click(cx.listener(
            move |this, _, _, cx| {
                this.s.convert_to_notes(clip);
                cx.notify();
            },
        )));

    div()
        .id("step-clip-scroll")
        .size_full()
        .overflow_x_scroll()
        .overflow_y_scroll()
        .p(px(SP3))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(SP2))
                .child(toolbar)
                .child(
                    div().flex().gap(px(SP3)).child(heads).child(
                        div().id("step-clip-grid").cursor_pointer().child(grid).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                let b = this.regions.step_grid.get();
                                let x = f32::from(e.position.x - b.origin.x);
                                let y = f32::from(e.position.y - b.origin.y);
                                let r = (y / (ROW + GAP)).floor() as usize;
                                let in_row = y - r as f32 * (ROW + GAP) < ROW;
                                let len = this.s.clip_pattern(clip).and_then(|p| p.rows.get(r)).map_or(0, |row| usize::from(row.length));
                                if let (true, Some(i)) = (in_row, step_at(x, len)) {
                                    this.s.toggle_pattern_step(clip, r, i);
                                    cx.notify();
                                }
                            }),
                        ),
                    ),
                )
                .child(div().flex().items_center().gap(px(SP3)).child(div().w(px(HEAD_W)).flex().items_center().child(lane_tabs)).child(
                    div()
                        .id("step-clip-lane")
                        .cursor_pointer()
                        .child(lane_canvas)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                                let b = this.regions.step_lane.get();
                                this.s.seal();
                                on_down(this, f32::from(e.position.x - b.origin.x), f32::from(e.position.y - b.origin.y), cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, _, cx| {
                            if e.pressed_button == Some(MouseButton::Left) {
                                let b = this.regions.step_lane.get();
                                on_move(this, f32::from(e.position.x - b.origin.x), f32::from(e.position.y - b.origin.y), cx);
                            }
                        }))
                        .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.s.seal()))
                        .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.s.seal())),
                )),
        )
        .when(pattern.rows.is_empty(), |d| d.child(label("No rows: add one with + Row.", TEXT_SM, pal.faint)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pointer_finds_the_step_under_it() {
        assert_eq!(step_at(0., 16), Some(0));
        assert_eq!(step_at(step_x(5) + 2., 16), Some(5));
        assert_eq!(step_at(step_x(3) + STEP + 1., 16), None, "the gap between steps");
        assert_eq!(step_at(step_x(16) + 1., 16), None, "past the row's length");
    }

    #[test]
    fn the_lane_writes_each_value_in_its_range() {
        let mut s = Step { on: true, ..Step::default() };
        Lane::Velocity.set(&mut s, 0.0);
        assert!(s.velocity >= 0.05);
        Lane::Chance.set(&mut s, 0.52);
        assert_eq!(s.chance, 0.5);
        Lane::Repeat.set(&mut s, 1.0);
        assert_eq!(s.repeat, MAX_REPEAT);
        Lane::Repeat.set(&mut s, 0.0);
        assert_eq!(s.repeat, 1);
        assert_eq!(Lane::Repeat.height(s), 1. / f32::from(MAX_REPEAT));
    }
}
