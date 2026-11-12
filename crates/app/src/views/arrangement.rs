//! Arrangement View: ruler with loop brace, track headers, and one canvas
//! that paints every lane (grid, clips with note minis or waveforms,
//! automation, playhead). Only what is on screen is painted.

use crate::icons::{Icon, icon};
use crate::paint::{self, at};
use crate::state::{AppState, BAR, Snap};
use crate::theme::*;
use crate::views::browser::kind_icon;
use crate::widgets::{self, chip, icon_chip, label, mono};
use crate::workspace::{Drag, MenuKind, Workspace};
use aitrack_model::*;
use gpui::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    Pixels, Point, ScrollDelta, ScrollWheelEvent, StatefulInteractiveElement, Styled, TextAlign, Window, canvas, div,
    point, prelude::FluentBuilder, px,
};

const AUTO_H: f32 = 56.;
const CLIP_HEAD: f32 = 14.;

#[derive(Clone, Debug)]
pub struct LaneRow {
    pub track: TrackId,
    pub kind: TrackKind,
    pub top: f32,
    pub h: f32,
    pub auto: Option<AutomationTarget>,
}

impl LaneRow {
    pub fn total(&self) -> f32 {
        self.h + if self.auto.is_some() { AUTO_H } else { 0. }
    }
}

/// The visible lanes, top to bottom (children of collapsed groups hidden).
pub fn rows(s: &AppState) -> Vec<LaneRow> {
    let mut out = Vec::new();
    let mut y = 0.;
    for t in s.project.content_tracks() {
        // Return tracks sit with Main in the pinned bus section at the bottom.
        if t.kind == TrackKind::Return {
            continue;
        }
        if let Some(parent) = t.parent
            && s.project.track(parent).is_some_and(|g| g.collapsed) {
                continue;
            }
        let auto = s.automation_shown.contains(&t.id).then(|| t.automation.first().map(|l| l.target)).flatten();
        let h = crate::theme::density(t.height.max(40.).round());
        out.push(LaneRow { track: t.id, kind: t.kind, top: y, h, auto });
        y += out.last().map_or(0., LaneRow::total);
    }
    out
}

fn total_height(rows: &[LaneRow]) -> f32 {
    rows.last().map_or(0., |r| r.top + r.total())
}

/// The screen-space line of a lane: its breakpoints, with samples along the
/// segments that are curved so the drawing follows what the engine plays.
fn automation_line(lane: &aitrack_model::AutomationLane, px_per_tick: f32, scroll_x: f32) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(lane.points.len());
    for (i, p) in lane.points.iter().enumerate() {
        out.push((p.tick as f32 * px_per_tick - scroll_x, p.value));
        if let Some(next) = lane.points.get(i + 1)
            && p.curve.abs() > 1e-3
        {
            for k in 1..16 {
                let tick = p.tick + (next.tick - p.tick) * k / 16;
                if let Some(v) = lane.value_at(tick) {
                    out.push((tick as f32 * px_per_tick - scroll_x, v));
                }
            }
        }
    }
    out
}

/// Normalised automation value under a window position in `track`'s lane.
pub fn automation_value_at(ws: &Workspace, track: TrackId, pos: Point<Pixels>) -> Option<f32> {
    let row = rows(&ws.s).into_iter().find(|r| r.track == track)?;
    let b = ws.regions.arrange.get();
    let y = f32::from(pos.y - b.origin.y) + ws.s.arrange.scroll_y - row.top - row.h;
    Some((1. - (y - 6.) / (AUTO_H - 12.)).clamp(0., 1.))
}

fn target_name(ws: &Workspace, t: AutomationTarget) -> String {
    let p = &ws.s.project;
    match t {
        AutomationTarget::Volume => "Volume".into(),
        AutomationTarget::Pan => "Pan".into(),
        AutomationTarget::Mute => "Mute".into(),
        AutomationTarget::Send { index } => format!("Send {}", (b'A' + index as u8) as char),
        AutomationTarget::DeviceParam { device, param } => {
            let d = p.device(device);
            let name = d.map_or("Device".into(), |d| d.name.clone());
            let pname = d
                .and_then(|d| match &d.kind {
                    DeviceKind::Stock { plugin_id } => ws.b.plugins.device(plugin_id),
                    _ => None,
                })
                .and_then(|i| i.params.iter().find(|p| p.id == param))
                .map_or(format!("P{param}"), |p| p.name.to_owned());
            format!("{name} · {pname}")
        }
        AutomationTarget::DeviceEnabled { .. } => "Device On".into(),
        AutomationTarget::Macro { index, .. } => format!("Macro {}", index + 1),
        AutomationTarget::Tempo => "Tempo".into(),
    }
}

enum Body {
    Notes { notes: Vec<(u8, Tick, Tick)>, lo: u8, hi: u8, loop_len: Tick },
    Audio { seed: u64 },
}

struct ClipPaint {
    x: f32,
    w: f32,
    y: f32,
    h: f32,
    color: Color,
    name: String,
    selected: bool,
    body: Body,
    px_per_tick: f32,
    offset: Tick,
    /// Deactivated with 0: drawn drained of colour.
    muted: bool,
}

struct AutoPaint {
    y: f32,
    points: Vec<(f32, f32)>,
    /// The lane as drawn: breakpoints plus samples along curved segments.
    line: Vec<(f32, f32)>,
    color: Hsla,
}

pub fn render(ws: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let rows = rows(&ws.s);
    let view = ws.s.arrange;
    let ppb = view.px_per_beat;
    let px_per_tick = ppb / PPQ as f32;
    let transport = ws.b.engine.transport();
    let lane_w = f32::from(ws.regions.arrange.get().size.width).max(1200.);
    let lane_h = f32::from(ws.regions.arrange.get().size.height).max(600.);
    let t0 = (view.scroll_x / px_per_tick) as Tick;
    let t1 = ((view.scroll_x + lane_w) / px_per_tick) as Tick;
    let song_end = ws.s.project.arrangement_end();
    let loop_region = ws.s.project.loop_region;

    // ── Gather what the lane canvas paints ──
    let mut clips = Vec::new();
    let mut autos = Vec::new();
    let mut lanes = Vec::new();
    for r in &rows {
        let Some(t) = ws.s.project.track(r.track) else { continue };
        let ry = r.top - view.scroll_y;
        lanes.push((ry, r.h, t.color, r.auto.is_some(), ws.s.selected_track == Some(t.id)));
        if ry > lane_h || ry + r.total() < 0. {
            continue;
        }
        for pl in &t.arrangement {
            let Some(c) = ws.s.project.clips.get(&pl.clip) else { continue };
            if pl.start + c.length < t0 || pl.start > t1 {
                continue;
            }
            let body = match ws.s.project.sources.get(&c.source) {
                Some(Source::Notes(n)) => {
                    let lo = n.notes.iter().map(|n| n.key).min().unwrap_or(60);
                    let hi = n.notes.iter().map(|n| n.key).max().unwrap_or(60);
                    Body::Notes {
                        notes: n.notes.iter().map(|n| (n.key, n.start, n.length)).collect(),
                        lo,
                        hi,
                        loop_len: c.looping.map_or(c.length, |l| l.end - l.start).max(1),
                    }
                }
                _ => Body::Audio { seed: c.source.0 },
            };
            clips.push(ClipPaint {
                x: pl.start as f32 * px_per_tick - view.scroll_x,
                w: c.length as f32 * px_per_tick,
                y: ry + 2.,
                h: r.h - 4.,
                color: c.color,
                name: c.name.clone(),
                selected: ws.s.selected_placement.is_some_and(|(_, id)| id == pl.id),
                body,
                px_per_tick,
                offset: c.offset,
                muted: c.muted,
            });
        }
        if let Some(target) = r.auto
            && let Some(lane) = t.automation.iter().find(|l| l.target == target) {
                autos.push(AutoPaint {
                    y: ry + r.h,
                    points: lane.points.iter().map(|p| (p.tick as f32 * px_per_tick - view.scroll_x, p.value)).collect(),
                    line: automation_line(lane, px_per_tick, view.scroll_x),
                    color: crate::theme::mix(track_color(t.color), p().automation, 0.35),
                });
            }
    }
    let playhead = transport.position as f32 * px_per_tick - view.scroll_x;
    let loop_px = loop_region.map(|(a, b)| (a as f32 * px_per_tick - view.scroll_x, b as f32 * px_per_tick - view.scroll_x));
    let end_px = song_end as f32 * px_per_tick - view.scroll_x;

    let regions = ws.regions.clone();
    let lane_canvas = canvas(
        move |b, _, _| regions.arrange.set(b),
        move |b, _, window, cx| {
            let o = b.origin;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            window.paint_quad(paint::fill_rect(b, p().well));
            // Lane backgrounds.
            for (y, lh, _, auto, sel) in &lanes {
                window.paint_quad(paint::fill_rect(at(o, 0., *y, w, *lh), if *sel { content(0.025) } else { gpui::transparent_black() }));
                window.paint_quad(paint::fill_rect(at(o, 0., *y + *lh - 1., w, 1.), border()));
                if *auto {
                    window.paint_quad(paint::fill_rect(at(o, 0., *y + *lh, w, AUTO_H), content(0.015)));
                    window.paint_quad(paint::fill_rect(at(o, 0., *y + *lh + AUTO_H - 1., w, 1.), border()));
                }
            }
            // Grid.
            let first_bar = (view.scroll_x / (ppb * 4.)).floor() as i64;
            let bars = (w / (ppb * 4.)).ceil() as i64 + 1;
            for i in 0..=bars {
                let bar = first_bar + i;
                let x = bar as f32 * ppb * 4. - view.scroll_x;
                if bar % 8 >= 4 {
                    window.paint_quad(paint::fill_rect(at(o, x, 0., ppb * 4., h), content(0.012)));
                }
                window.paint_quad(paint::fill_rect(at(o, x, 0., 1., h), if bar % 4 == 0 { p().grid_bar } else { p().grid_beat }));
                if ppb >= 14. {
                    for beat in 1..4 {
                        window.paint_quad(paint::fill_rect(at(o, x + beat as f32 * ppb, 0., 1., h), p().grid_sub));
                    }
                }
            }
            if let Some((a, bb)) = loop_px {
                window.paint_quad(paint::fill_rect(at(o, a, 0., bb - a, h), accent_alpha(0.035)));
            }
            if end_px > 0. && end_px < w {
                window.paint_quad(paint::fill_rect(at(o, end_px, 0., 1., h), content(0.18)));
            }
            // Clips.
            for c in &clips {
                paint_clip(window, cx, o, c);
            }
            // Automation.
            for a in &autos {
                let lane_top = a.y + 6.;
                let lane_h = AUTO_H - 12.;
                let mut pts: Vec<Point<Pixels>> = Vec::with_capacity(a.points.len() + 2);
                if let Some(first) = a.points.first() {
                    pts.push(point(o.x, o.y + px(lane_top + lane_h * (1. - first.1))));
                }
                for (x, v) in &a.line {
                    pts.push(point(o.x + px(*x), o.y + px(lane_top + lane_h * (1. - v))));
                }
                if let Some(last) = a.points.last() {
                    pts.push(point(o.x + px(w), o.y + px(lane_top + lane_h * (1. - last.1))));
                }
                paint::polyline(window, &pts, 1.5, a.color);
                for (x, v) in &a.points {
                    let c = point(o.x + px(*x), o.y + px(lane_top + lane_h * (1. - v)));
                    window.paint_quad(paint::rrect_border(
                        gpui::Bounds::new(point(c.x - px(3.5), c.y - px(3.5)), gpui::size(px(7.), px(7.))),
                        3.5,
                        p().well,
                        a.color,
                        1.5,
                    ));
                }
            }
            // Playhead.
            if playhead >= 0. && playhead <= w {
                window.paint_quad(paint::fill_rect(at(o, playhead, 0., 1., h), accent()));
            }
        },
    )
    .size_full();

    let lanes_el = div()
        .id("arrange-lanes")
        .relative()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .overflow_hidden()
        .child(lane_canvas)
        .on_mouse_down(MouseButton::Left, cx.listener(lane_mouse_down))
        .on_mouse_down(MouseButton::Right, cx.listener(lane_right_click))
        .drag_over::<crate::workspace::DraggedSample>(|s, _, _, _| s.bg(accent_alpha(0.06)))
        // A library sample dropped on a lane lands on that track (audio
        // tracks; otherwise a new one) at the beat under the cursor.
        .on_drop(cx.listener(|this, d: &crate::workspace::DraggedSample, window, cx| {
            let pos = window.mouse_position();
            let (tick, row) = this.arrange_pos(pos);
            let start = crate::state::Snap::Beat.floor(tick.max(0));
            this.add_sample_clip(&d.0, row, Some(start), cx);
        }))
        .on_scroll_wheel(cx.listener(on_wheel));

    // ── Track headers ──
    let mut headers = div().relative().w_full().top(px(-view.scroll_y)).flex().flex_col();
    for r in &rows {
        headers = headers.child(track_header(ws, r, cx));
    }
    let header_col = div()
        .id("arrange-headers")
        .flex_none()
        .w(px(TRACK_HEADER_W))
        .h_full()
        .overflow_hidden()
        .border_r_1()
        .border_color(border())
        .bg(pal.surface)
        .child(headers)
        .on_scroll_wheel(cx.listener(on_wheel));

    // ── Ruler ──
    let regions = ws.regions.clone();
    let ruler = canvas(
        move |b, _, _| regions.ruler.set(b),
        move |b, _, window, cx| {
            let o = b.origin;
            let w = f32::from(b.size.width);
            let h = f32::from(b.size.height);
            window.paint_quad(paint::fill_rect(b, p().surface));
            window.paint_quad(paint::fill_rect(at(o, 0., h - 1., w, 1.), border()));
            if let Some((a, bb)) = loop_px {
                window.paint_quad(paint::rrect(at(o, a, 3., bb - a, 8.), 3., accent_alpha(0.75)));
            }
            let bar_px = ppb * 4.;
            let step = [1, 2, 4, 8, 16].into_iter().find(|s| *s as f32 * bar_px >= 40.).unwrap_or(32);
            let first = (view.scroll_x / bar_px).floor() as i64;
            let count = (w / bar_px).ceil() as i64 + 1;
            for i in 0..=count {
                let bar = first + i;
                if bar < 0 || bar % step != 0 {
                    continue;
                }
                let x = bar as f32 * bar_px - view.scroll_x;
                window.paint_quad(paint::fill_rect(at(o, x, h - 10., 1., 10.), content(0.3)));
                paint::text(window, cx, &format!("{}", bar + 1), o.x + px(x + 4.), o.y + px(12.), TEXT_XS, p().muted, FontWeight::MEDIUM, TextAlign::Left);
            }
            if playhead >= 0. && playhead <= w {
                paint::polygon(
                    window,
                    &[point(o.x + px(playhead - 5.), o.y + px(h - 8.)), point(o.x + px(playhead + 5.), o.y + px(h - 8.)), point(o.x + px(playhead), o.y + px(h - 1.))],
                    accent(),
                );
            }
        },
    )
    .size_full();
    let ruler_el = div()
        .id("ruler")
        .flex_1()
        .h_full()
        .cursor_pointer()
        .child(ruler)
        .tooltip(widgets::tip("Click to locate · drag the top strip to set the loop", None))
        .on_mouse_down(MouseButton::Left, cx.listener(|this, e: &MouseDownEvent, _, cx| {
            let b = this.regions.ruler.get();
            let y = f32::from(e.position.y - b.origin.y);
            let x = f32::from(e.position.x - b.origin.x) + this.s.arrange.scroll_x;
            let tick = (x / this.s.arrange.px_per_beat * PPQ as f32) as Tick;
            if y < 12. {
                let anchor = Snap::Bar.round(tick).max(0);
                this.begin_drag(Drag::LoopBrace { anchor }, cx);
            } else {
                this.b.engine.locate(Snap::Beat.floor(tick.max(0)));
                this.begin_drag(Drag::Scrub, cx);
            }
        }))
        .on_scroll_wheel(cx.listener(on_wheel));

    let toolbar = div()
        .flex_none()
        .w(px(TRACK_HEADER_W))
        .h_full()
        .px(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP1))
        .bg(pal.surface)
        .border_r_1()
        .border_b_1()
        .border_color(border())
        .child(
            div()
                .id("arr-snap")
                .h(px(24.))
                .px(px(SP2))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_md()))
                .cursor_pointer()
                .hover(|s| s.bg(hover()))
                .tooltip(widgets::tip("Snap grid", None))
                .on_click(cx.listener(|this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.open_menu(MenuKind::Snap, point(at.x - px(16.), at.y + px(14.)), cx);
                }))
                .child(icon(Icon::Magnet, 14., if ws.s.snap == Snap::Off { pal.faint } else { pal.accent }))
                .child(label(ws.s.snap.label(), TEXT_SM, pal.text)),
        )
        .child(div().flex_1())
        .child(widgets::sized_icon_button("zoom-out", Icon::ZoomOut, false, 24., 14.).tooltip(widgets::tip("Zoom out", Some("Ctrl+Wheel"))).on_click(cx.listener(|this, _, _, cx| {
            this.s.arrange.px_per_beat = (this.s.arrange.px_per_beat / 1.25).max(4.);
            cx.notify();
        })))
        .child(widgets::sized_icon_button("zoom-in", Icon::ZoomIn, false, 24., 14.).tooltip(widgets::tip("Zoom in", Some("Ctrl+Wheel"))).on_click(cx.listener(|this, _, _, cx| {
            this.s.arrange.px_per_beat = (this.s.arrange.px_per_beat * 1.25).min(160.);
            cx.notify();
        })));

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(div().flex_none().h(px(RULER_H)).flex().child(toolbar).child(ruler_el))
        .child(div().flex_1().min_h(px(0.)).flex().child(header_col).child(lanes_el))
        .child(bus_section(ws, cx))
        .into_any_element()
}

/// Height of one row of the pinned bus section.
const BUS_H: f32 = 28.;

/// Return tracks and Main, pinned under the lanes so the whole signal path
/// is always in view: tracks → returns → Main. Clicking one selects it, so
/// its device chain (bus effects, mastering) opens in the Devices tab.
fn bus_section(ws: &mut Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let buses: Vec<aitrack_model::Track> =
        ws.s.project.tracks.iter().filter(|t| matches!(t.kind, TrackKind::Return | TrackKind::Main)).cloned().collect();
    let mut headers = div().flex_none().w(px(TRACK_HEADER_W)).flex().flex_col().border_r_1().border_color(border());
    let mut lanes = div().flex_1().min_w(px(0.)).flex().flex_col();
    let mut letter = b'A';
    for t in buses {
        let id = t.id;
        let is_main = t.kind == TrackKind::Main;
        let selected = ws.s.selected_track == Some(id);
        let (mute, solo) = (t.mute, t.solo);
        let tag = if is_main {
            "Main".to_owned()
        } else {
            letter += 1;
            format!("{}", (letter - 1) as char)
        };
        let effects = t.devices.len();
        headers = headers.child(
            div()
                .id(("bus-head", id.0 as usize))
                .h(px(BUS_H))
                .flex()
                .items_center()
                .gap(px(SP2))
                .pr(px(SP2))
                .bg(if selected { pal.raised } else { pal.surface })
                .border_b_1()
                .border_color(border())
                .cursor_pointer()
                .tooltip(widgets::tip(if is_main { "Main output: every track and return ends here" } else { "Return bus: fed by the tracks' sends" }, None))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.s.selected_track = Some(id);
                    cx.notify();
                }))
                .child(div().w(px(4.)).h_full().bg(track_color(t.color)))
                .child(icon(if is_main { Icon::Volume } else { Icon::Return }, 14., pal.muted))
                .child(label(paint::fit(&t.name, TRACK_HEADER_W - 150., TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM).flex_1())
                .child(chip(("bus-m", id.0 as usize), "M", mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.s.set_track(id, TrackProp::Mute(!mute));
                    cx.notify();
                })))
                .when(!is_main, |d| {
                    d.child(chip(("bus-s", id.0 as usize), "S", solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.s.set_track(id, TrackProp::Solo(!solo));
                        cx.notify();
                    })))
                })
                .child(mono(crate::state::format_db(t.volume_db), TEXT_XS, pal.muted).w(px(34.)))
                .child(widgets::meter(ws.b.engine.meter(id), 24., 6.)),
        );
        lanes = lanes.child(
            div()
                .h(px(BUS_H))
                .px(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .bg(if selected { accent_alpha(0.05) } else { content(0.02) })
                .border_b_1()
                .border_color(border())
                .child(widgets::badge(tag, track_color(t.color)))
                .child(label(
                    match effects {
                        0 => "no effects".to_owned(),
                        1 => "1 effect".to_owned(),
                        n => format!("{n} effects"),
                    },
                    TEXT_XS,
                    pal.faint,
                )),
        );
    }
    div().flex_none().flex().border_t_1().border_color(border_strong()).bg(pal.surface).child(headers).child(lanes).into_any_element()
}

fn paint_clip(window: &mut Window, cx: &mut gpui::App, o: Point<Pixels>, c: &ClipPaint) {
    let body = at(o, c.x + 1., c.y, (c.w - 2.).max(2.), c.h);
    let fill = if c.muted { mix(clip_fill(c.color), p().well, 0.7) } else { clip_fill(c.color) };
    let fg = if c.muted { p().muted } else { clip_text(c.color) };
    window.paint_quad(paint::rrect_border(body, r_sm(), with_alpha(fill, 0.55), if c.selected { accent() } else { with_alpha(fill, 1.) }, if c.selected { 1.5 } else { 1. }));
    window.paint_quad(paint::rrect(at(o, c.x + 1., c.y, (c.w - 2.).max(2.), CLIP_HEAD), r_sm(), fill));
    if c.w > 28. {
        window.with_content_mask(Some(gpui::ContentMask { bounds: body }), |window| {
            paint::text(window, cx, &paint::fit(&c.name, c.w - 10., TEXT_XS), o.x + px(c.x + 5.), o.y + px(c.y + 0.5), TEXT_XS, fg, FontWeight::MEDIUM, TextAlign::Left);
        });
    }
    let inner_y = c.y + CLIP_HEAD + 2.;
    let inner_h = c.h - CLIP_HEAD - 4.;
    if inner_h < 4. {
        return;
    }
    window.with_content_mask(Some(gpui::ContentMask { bounds: body }), |window| match &c.body {
        Body::Notes { notes, lo, hi, loop_len } => {
            let span = (*hi as f32 - *lo as f32 + 1.).max(8.);
            let row = (inner_h / span).clamp(1.5, 5.);
            let used = row * (*hi as f32 - *lo as f32 + 1.);
            let base = inner_y + (inner_h - used) / 2.;
            let reps = ((c.w / c.px_per_tick) / *loop_len as f32).ceil() as i64;
            for rep in 0..reps.max(1) {
                let ox = c.x + (rep * loop_len) as f32 * c.px_per_tick - c.offset as f32 * c.px_per_tick;
                if rep > 0 {
                    window.paint_quad(paint::fill_rect(at(o, ox, inner_y, 1., inner_h), with_alpha(fg, 0.15)));
                }
                for (k, s, l) in notes {
                    if *s >= *loop_len {
                        continue;
                    }
                    let nx = ox + *s as f32 * c.px_per_tick;
                    let ny = base + (*hi - *k) as f32 * row;
                    window.paint_quad(paint::fill_rect(at(o, nx, ny, (*l as f32 * c.px_per_tick - 1.).max(1.), (row - 0.5).max(1.)), with_alpha(fg, 0.85)));
                }
            }
        }
        Body::Audio { seed } => {
            // Placeholder waveform until the engine reports real peaks.
            let mid = inner_y + inner_h / 2.;
            let n = ((c.w - 2.) / 2.).max(2.) as usize;
            let mut top = Vec::with_capacity(n * 2);
            let mut s = *seed as f32 * 0.37;
            for i in 0..n {
                let x = c.x + 1. + i as f32 * 2.;
                let beat = (i as f32 * 2.) / (c.px_per_tick * PPQ as f32);
                let env = 0.35 + 0.55 * (-(beat.fract()) * 2.2).exp();
                s = (s * 1.3 + 0.618).fract();
                let a = (env * (0.6 + 0.4 * s) * inner_h / 2.).max(0.5);
                top.push(point(o.x + px(x), o.y + px(mid - a)));
            }
            let bottom: Vec<_> = top.iter().rev().map(|p| point(p.x, o.y + px(2. * mid) - (p.y - o.y))).collect();
            let wave = crate::theme::mix(fg, p().waveform, 0.5);
            if style().waveform == WaveformStyle::Outline {
                let mut ring = top.clone();
                ring.extend(bottom);
                if let Some(first) = ring.first().copied() {
                    ring.push(first);
                }
                paint::polyline(window, &ring, 1., wave);
            } else {
                top.extend(bottom);
                paint::polygon(window, &top, wave);
            }
        }
    });
}

fn track_header(ws: &mut Workspace, r: &LaneRow, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(t) = ws.s.project.track(r.track).cloned() else { return div().into_any_element() };
    let id = t.id;
    let selected = ws.s.selected_track == Some(id);
    let auto_on = ws.s.automation_shown.contains(&id);
    let has_auto = !t.automation.is_empty();
    let is_group = t.kind == TrackKind::Group;
    let (mute, solo, arm, collapsed) = (t.mute, t.solo, t.arm, t.collapsed);
    let indent = if t.parent.is_some() { SP3 } else { 0. };
    let main = div()
        .id(("arr-head", id.0 as usize))
        .h(px(r.h))
        .flex()
        .bg(if selected { pal.raised } else { pal.surface })
        .border_b_1()
        .border_color(border())
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.s.selected_track = Some(id);
            cx.notify();
        }))
        .child(div().w(px(4.)).h_full().bg(track_color(t.color)))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .pl(px(SP2 + indent))
                .pr(px(SP2))
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(SP1))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP2))
                        .when(is_group, |d| {
                            d.child(
                                widgets::sized_icon_button(("fold", id.0 as usize), if collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, false, 16., 12.)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.s.set_track(id, TrackProp::Collapsed(!collapsed));
                                        cx.notify();
                                    })),
                            )
                        })
                        .child(icon(if crate::state::is_drum_track(&t) { Icon::Drum } else { kind_icon(t.kind) }, 14., pal.muted))
                        .child(label(paint::fit(&t.name, TRACK_HEADER_W - 96. - indent, TEXT_MD), TEXT_MD, pal.text).font_weight(FontWeight::MEDIUM).flex_1())
                        .child(mono(crate::state::format_db(t.volume_db), TEXT_XS, pal.muted)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(chip(("a-m", id.0 as usize), "M", mute, pal.mute).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.set_track(id, TrackProp::Mute(!mute));
                            cx.notify();
                        })))
                        .child(chip(("a-s", id.0 as usize), "S", solo, pal.solo).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.s.set_track(id, TrackProp::Solo(!solo));
                            cx.notify();
                        })))
                        .when(t.kind.has_clips(), |d| {
                            d.child(chip(("a-r", id.0 as usize), "R", arm, pal.record).on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.s.set_track(id, TrackProp::Arm(!arm));
                                cx.notify();
                            })))
                        })
                        .child(div().flex_1())
                        .when(has_auto, |d| {
                            d.child(
                                icon_chip(("a-auto", id.0 as usize), Icon::Automation, auto_on, pal.accent)
                                    .tooltip(widgets::tip("Show automation", None))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        if !this.s.automation_shown.remove(&id) {
                                            this.s.automation_shown.insert(id);
                                        }
                                        cx.notify();
                                    })),
                            )
                        })
                        .child(widgets::meter(ws.b.engine.meter(id), 24., 6.)),
                ),
        );
    let mut col = div().flex().flex_col().child(main);
    if let Some(target) = r.auto {
        col = col.child(
            div()
                .h(px(AUTO_H))
                .pl(px(SP2 + 4. + indent))
                .pr(px(SP2))
                .flex()
                .items_center()
                .gap(px(SP2))
                .bg(content(0.02))
                .border_b_1()
                .border_color(border())
                .child(icon(Icon::Automation, 12., pal.faint))
                .child(label(paint::fit(&target_name(ws, target), TRACK_HEADER_W - 48., TEXT_SM), TEXT_SM, pal.muted)),
        );
    }
    col.into_any_element()
}

fn lane_hit(ws: &Workspace, pos: Point<Pixels>) -> Option<(LaneRow, Tick, f32, f32)> {
    let b = ws.regions.arrange.get();
    let x = f32::from(pos.x - b.origin.x) + ws.s.arrange.scroll_x;
    let y = f32::from(pos.y - b.origin.y) + ws.s.arrange.scroll_y;
    let tick = (x / ws.s.arrange.px_per_beat * PPQ as f32) as Tick;
    rows(&ws.s).into_iter().find(|r| y >= r.top && y < r.top + r.total()).map(|r| {
        let ly = y - r.top;
        (r, tick, x, ly)
    })
}

fn lane_mouse_down(ws: &mut Workspace, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    if e.first_mouse {
        return;
    }
    let Some((row, tick, x, ly)) = lane_hit(ws, e.position) else {
        ws.s.selected_placement = None;
        cx.notify();
        return;
    };
    let ppt = ws.s.arrange.px_per_beat / PPQ as f32;
    let track = row.track;
    ws.s.selected_track = Some(track);
    // Automation lane.
    if ly >= row.h {
        if let Some(target) = row.auto {
            let lane = ws.s.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target).cloned());
            let points = lane.map(|l| l.points).unwrap_or_default();
            let value = (1. - (ly - row.h - 6.) / (AUTO_H - 12.)).clamp(0., 1.);
            let hit = points.iter().position(|p| (p.tick as f32 * ppt - x).abs() < 6. && ((1. - p.value) * (AUTO_H - 12.) + 6. - (ly - row.h)).abs() < 8.);
            // Double-click a breakpoint to delete it (as in Live).
            if let Some(i) = hit.filter(|_| e.click_count >= 2) {
                ws.s.delete_automation_point(track, target, i);
                cx.notify();
                return;
            }
            // Alt + drag between two breakpoints bends that segment.
            if hit.is_none() && e.modifiers.alt {
                if let Some(i) = points.windows(2).position(|w| tick >= w[0].tick && tick < w[1].tick) {
                    let (a, b) = (points[i], points[i + 1]);
                    ws.s.seal();
                    ws.begin_drag(Drag::AutoCurve { track, target, index: i, start_y: f32::from(e.position.y), start_curve: a.curve, rising: b.value >= a.value }, cx);
                }
                return;
            }
            let index = match hit {
                Some(i) => i,
                None => {
                    let t = ws.s.snap.round(tick);
                    ws.s.set_automation_point(track, target, t, value, false);
                    let pts = ws.s.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target)).map(|l| l.points.clone()).unwrap_or_default();
                    pts.iter().position(|p| p.tick == t).unwrap_or(0)
                }
            };
            let original = ws.s.project.track(track).and_then(|t| t.automation.iter().find(|l| l.target == target)).map(|l| l.points.clone()).unwrap_or_default();
            ws.begin_drag(Drag::AutoPoint { track, target, original, index }, cx);
        }
        return;
    }
    // Clips.
    let hit = ws.s.project.track(track).and_then(|t| {
        t.arrangement.iter().rev().find_map(|pl| {
            let c = ws.s.project.clips.get(&pl.clip)?;
            (tick >= pl.start && tick < pl.start + c.length).then_some((pl.id, pl.clip, pl.start, c.length))
        })
    });
    match hit {
        Some((pid, clip, start, length)) => {
            ws.s.selected_placement = Some((track, pid));
            ws.s.select_clip(Some(clip));
            if e.click_count >= 2 {
                let tab = if ws.s.clip_notes(clip).is_some() { crate::state::BottomTab::PianoRoll } else { crate::state::BottomTab::Devices };
                ws.set_tab(tab, cx);
                return;
            }
            let right_edge = (start + length) as f32 * ppt;
            if right_edge - x < 8. {
                ws.begin_drag(Drag::ClipResize { clip, length, grab_tick: tick }, cx);
            } else {
                ws.begin_drag(Drag::Clip { track, placement: pid, start, grab_tick: tick, applied: (start, track) }, cx);
            }
        }
        None => {
            ws.s.selected_placement = None;
            if e.click_count >= 2 && row.kind == TrackKind::Instrument {
                create_clip(ws, track, Snap::Bar.floor(tick));
            } else if !ws.b.engine.transport().playing {
                ws.b.engine.locate(Snap::Beat.floor(tick.max(0)));
            }
            cx.notify();
        }
    }
}

fn lane_right_click(ws: &mut Workspace, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    let Some((row, tick, x, ly)) = lane_hit(ws, e.position) else { return };
    if ly >= row.h {
        // Right-click a breakpoint to delete it.
        if let Some(target) = row.auto {
            let ppt = ws.s.arrange.px_per_beat / PPQ as f32;
            let points = ws.s.project.track(row.track).and_then(|t| t.automation.iter().find(|l| l.target == target)).map(|l| l.points.clone()).unwrap_or_default();
            let hit = points.iter().position(|p| (p.tick as f32 * ppt - x).abs() < 7. && ((1. - p.value) * (AUTO_H - 12.) + 6. - (ly - row.h)).abs() < 9.);
            if let Some(i) = hit {
                ws.s.delete_automation_point(row.track, target, i);
                cx.notify();
            }
        }
        return;
    }
    let hit = ws.s.project.track(row.track).and_then(|t| {
        t.arrangement.iter().find(|pl| ws.s.project.clips.get(&pl.clip).is_some_and(|c| tick >= pl.start && tick < pl.start + c.length)).map(|pl| pl.id)
    });
    if let Some(pid) = hit {
        ws.s.remove_placement(row.track, pid);
        ws.toast("Clip deleted — Ctrl+Z to undo", cx);
    }
}

/// Double-click on an empty MIDI lane: a new one-bar clip.
fn create_clip(ws: &mut Workspace, track: TrackId, start: Tick) {
    let color = ws.s.project.track(track).map_or(Color::palette(0), |t| t.color);
    let src = ws.s.project.alloc_source_id();
    let clip = ws.s.project.alloc_clip_id();
    let pl = ws.s.project.alloc_placement_id();
    let c = Clip {
        name: "New Clip".into(),
        color,
        source: src,
        offset: 0,
        length: BAR,
        looping: None,
        gain_db: 0.,
        envelopes: Vec::new(),
        launch: LaunchSettings::default(),
        muted: false,
        scale: None,
    };
    if ws.s.edit("Add clip", vec![
        Command::AddSource { id: src, source: Box::new(Source::Notes(NoteSource::default())) },
        Command::AddClip { id: clip, clip: Box::new(c) },
        Command::PlaceClip { track, placement: Placement { id: pl, clip, start: start.max(0), lane: 0 } },
    ]) {
        ws.s.selected_placement = Some((track, pl));
        ws.s.select_clip(Some(clip));
    }
}

fn on_wheel(ws: &mut Workspace, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Workspace>) {
    let d = match e.delta {
        ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(l) => (l.x * 40., l.y * 40.),
    };
    let v = &mut ws.s.arrange;
    if e.modifiers.secondary() || e.modifiers.control {
        let b = ws.regions.arrange.get();
        let mx = f32::from(e.position.x - b.origin.x).max(0.);
        let tick_at = (mx + v.scroll_x) / v.px_per_beat;
        v.px_per_beat = (v.px_per_beat * (1. + d.1 / 400.)).clamp(4., 160.);
        v.scroll_x = (tick_at * v.px_per_beat - mx).max(0.);
    } else if e.modifiers.shift || d.0.abs() > d.1.abs() {
        let dx = if d.0.abs() > d.1.abs() { d.0 } else { d.1 };
        v.scroll_x = (v.scroll_x - dx).max(0.);
    } else {
        let total = total_height(&rows(&ws.s));
        let view_h = f32::from(ws.regions.arrange.get().size.height);
        let v = &mut ws.s.arrange;
        v.scroll_y = (v.scroll_y - d.1).clamp(0., (total - view_h + 48.).max(0.));
    }
    cx.notify();
}
