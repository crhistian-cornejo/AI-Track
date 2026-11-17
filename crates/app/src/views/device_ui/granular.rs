//! `instrument.granular`: the source's waveform across the whole editor with
//! every grain that is sounding drawn on it, and the controls underneath.
//!
//! The device's meter (264 values, see the plugin) carries the source's
//! waveform, the grains that are alive (position, age, pan) and the state of
//! the buffer, so the picture is of what the device is doing now:
//!
//! * a grain is a bar on its place in the source, as long as the grain is,
//!   fading with its age, higher or lower by its pan (left up, right down);
//! * the **position** handle (centre line) moves where grains are read from,
//!   the **spray** handle on the top band's edge widens the range they are
//!   drawn from, the **scan** handle at the bottom is the reach of the scan
//!   LFO (the arrow says which way the scan travels) and the handle on the
//!   right edge sets how far the pan spreads them;
//! * with the Live source the stage shows how full the buffer is, and Freeze
//!   is a button of its own beside the source.
//!
//! Under the stage the controls are grouped by what they do. The ones used
//! all the time are always there; the rest (envelope, filter, the secondary
//! grain and pitch settings, the output's extras) open with **More**.
//!
//! The device's meter is empty with the simulated engine, so then the stage
//! says what to do instead of drawing grains.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, set_now};
use crate::paint;
use crate::theme::*;
use crate::views::devices::knob_control_sized;
use crate::widgets::{self, label, mono};
use crate::workspace::{KnobTarget, Workspace};
use aitrack_dsp::ParamScale;
use aitrack_model::DeviceId;
use aitrack_stock::instrument::granular::{METER_LEN, SOURCE_BUILTIN, SOURCE_LIVE, SOURCES, p};
use gpui::{
    AnyElement, Context, Div, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, StatefulInteractiveElement, Styled, TextAlign, canvas, div,
    prelude::FluentBuilder, px,
};
use std::cell::RefCell;
use std::collections::HashSet;

/// Diameter of a knob and the width of the cell it sits in.
const DIAL: f32 = 44.;
const CELL: f32 = 68.;
/// Grains the meter reports, and the values of each.
const GRAINS: usize = 64;
const WAVE_AT: usize = 8;
const GRAINS_AT: usize = WAVE_AT + 64;
/// The stage's least height in a window before the body scrolls.
const STAGE_MIN: f32 = 280.;
/// The share of the stage's height the grains' pan spans either way of the middle.
const PAN_REACH: f32 = 0.38;

thread_local! {
    /// Devices whose "More" drawer is open.
    static MORE: RefCell<HashSet<DeviceId>> = RefCell::new(HashSet::new());
}

// ───────────────────────────── What the meter says ─────────────────────────────

/// One grain that is sounding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grain {
    /// Where it reads from, 0..1 over the part of the source a grain fits.
    pub pos: f32,
    /// How far it is through its life, 0..1.
    pub age: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
}

/// The device's state as its meter reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub voices: usize,
    pub grains: Vec<Grain>,
    pub peak: f32,
    pub last: f32,
    pub frozen: bool,
    /// 0 sample, 1 live, 2 built-in.
    pub source: usize,
    pub secs: f32,
    pub fill: f32,
    pub wave: Vec<f32>,
}

/// Read the meter; `None` while it is not published (no engine, or no note yet).
pub fn read(meter: &[f32]) -> Option<View> {
    if meter.len() < METER_LEN {
        return None;
    }
    let grains = (0..GRAINS)
        .map(|k| Grain { pos: meter[GRAINS_AT + 3 * k], age: meter[GRAINS_AT + 3 * k + 1], pan: meter[GRAINS_AT + 3 * k + 2] })
        .filter(|g| g.pos >= 0.)
        .collect();
    Some(View {
        voices: meter[0].round().max(0.) as usize,
        grains,
        peak: meter[2],
        last: meter[3],
        frozen: meter[4] >= 0.5,
        source: meter[5].round().max(0.) as usize,
        secs: meter[6].max(0.),
        fill: meter[7].clamp(0., 1.),
        wave: meter[WAVE_AT..WAVE_AT + 64].to_vec(),
    })
}

/// The share of the source's length one grain of `size_ms` covers (0 when
/// the length is not known yet).
pub fn grain_share(size_ms: f32, secs: f32) -> f32 {
    if secs <= 0. { 0. } else { (size_ms / 1000. / secs).clamp(0., 0.9) }
}

/// Where along the source (0..1) a grain of `share` read at `pos` (the
/// device's 0..1 position) is centred: position 100 % is the last grain
/// that still fits, so the centre runs from half a grain to half a grain
/// before the end.
pub fn centre(pos: f32, share: f32) -> f32 {
    share / 2. + pos.clamp(0., 1.) * (1. - share)
}

/// The plot height fraction (0 = top) a grain of `pan` sits at, in a stage
/// whose middle is 0.5.
pub fn pan_y(pan: f32) -> f32 {
    0.5 + pan.clamp(-1., 1.) * PAN_REACH
}

/// The grain window as the device reads it: `kind` (Hann, Tukey, Gauss,
/// Triangle, Blackman, Sine), its peak moved by `shape` (percent, 100 = early)
/// and `u` the way through the grain.
pub fn window_value(kind: usize, shape: f32, u: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let peak = (0.1 + 0.8 * (1. - shape / 100.)).clamp(0.1, 0.9);
    let u = u.clamp(0., 1.);
    let x = if u < peak { u * 0.5 / peak } else { 0.5 + (u - peak) * 0.5 / (1. - peak) };
    let v = match kind {
        0 => 0.5 - 0.5 * (TAU * x).cos(),
        1 => {
            let a = 0.5;
            if x < a * 0.5 {
                0.5 - 0.5 * (TAU * x / a).cos()
            } else if x > 1. - a * 0.5 {
                0.5 - 0.5 * (TAU * (1. - x) / a).cos()
            } else {
                1.
            }
        }
        2 => {
            let g = |x: f32| (-0.5 * ((x - 0.5) / 0.17).powi(2)).exp();
            let e = g(0.);
            (g(x) - e) / (1. - e)
        }
        3 => 1. - (2. * x - 1.).abs(),
        4 => 0.42 - 0.5 * (TAU * x).cos() + 0.08 * (2. * TAU * x).cos(),
        _ => (PI * x).sin(),
    };
    v.max(0.)
}

/// `4.0 s`, `850 ms`.
fn fmt_secs(s: f32) -> String {
    if s >= 1. { format!("{s:.1} s") } else { format!("{:.0} ms", s * 1000.) }
}

// ───────────────────────────── Controls ─────────────────────────────

/// A knob with its name above and its value below.
fn knob(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    let pal = p();
    let value = dev.norm(id);
    let target = KnobTarget::Param { device: dev.id, param: id, default: info.default_normalized() };
    let k = knob_control_sized(("gr-knob", dev.key(id as usize)), value, info.min < 0. && info.max > 0., dev.enabled, DIAL, target, cx)
        .tooltip(widgets::tip(info.name, Some("Drag · Shift fine · double-click resets")));
    div()
        .flex_none()
        .w(px(CELL))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(SP1))
        .child(label(paint::fit(info.short, CELL, TEXT_SM), TEXT_SM, pal.muted))
        .child(k)
        .child(mono(paint::fit(&info.format(value), CELL, TEXT_SM), TEXT_SM, pal.text))
        .into_any_element()
}

/// Every choice as a chip, the current one lit, under its name.
fn chips(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    chips_named(dev, id, true, cx)
}

fn chips_named(dev: &Dev, id: u32, named: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    let pal = p();
    let (now, device) = (dev.choice(id), dev.id);
    let mut row = div().flex().items_center().gap(px(SP1));
    for (i, name) in info.choices.iter().enumerate() {
        let on = i == now;
        let norm = info.to_normalized(info.min + i as f32);
        row = row.child(
            div()
                .id(("gr-chip", dev.key((id as usize) << 4 | i)))
                .flex_none()
                .h(px(28.))
                .px(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.22) } else { content(0.05) })
                .hover(|s| s.bg(if on { accent_alpha(0.3) } else { content(0.1) }))
                .child(label(*name, TEXT_SM, if on { pal.accent } else { pal.text }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    set_now(this, device, id, norm);
                    cx.notify();
                })),
        );
    }
    div().flex_none().flex().flex_col().gap(px(SP1)).when(named, |d| d.child(label(info.name, TEXT_SM, pal.muted))).child(row).into_any_element()
}

/// The right control for a parameter: chips for up to four choices, a menu
/// for more, a knob otherwise.
fn ctl(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    match dev.info(id) {
        Some(i) if matches!(i.scale, ParamScale::Stepped) && !i.choices.is_empty() => {
            if i.choices.len() <= 4 { chips(dev, id, cx) } else { kit::pick(dev, id, cx) }
        }
        Some(_) => knob(dev, id, cx),
        None => div().into_any_element(),
    }
}

/// A titled group of controls: a card without a border, its title in capitals.
fn group(title: &str, body: Vec<AnyElement>) -> Div {
    div()
        .flex_none()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_md()))
        .bg(content(0.03))
        .child(label(title.to_uppercase(), TEXT_SM, p().muted).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1().flex().flex_wrap().items_end().gap(px(SP2)).children(body))
}

/// The button that stops the live recording and keeps what it holds.
fn freeze_button(dev: &Dev, live: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let on = dev.on(p::FREEZE);
    let (device, info) = (dev.id, dev.info(p::FREEZE).cloned());
    let ice = pal.accent2;
    div()
        .id(("gr-freeze", dev.key(0)))
        .flex_none()
        .w(px(120.))
        .h(px(40.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(SP2))
        .rounded(px(r_md()))
        .cursor_pointer()
        .border_1()
        .border_color(if on { ice } else { border_strong() })
        .bg(if on { with_alpha(ice, 0.28) } else { content(0.06) })
        .hover(|s| s.bg(if on { with_alpha(ice, 0.36) } else { content(0.12) }))
        .tooltip(widgets::tip(if live { "Stop recording and keep what the buffer holds" } else { "Hold the grains where they are (the scan stops)" }, None))
        .child(div().size(px(SP2)).rounded_full().bg(if on { ice } else { pal.faint }))
        .child(label(if on { "FROZEN" } else { "FREEZE" }, TEXT_SM + 1., if on { pal.text } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(info) = &info {
                set_now(this, device, p::FREEZE, info.to_normalized(f32::from(!on)));
            }
            cx.notify();
        }))
        .into_any_element()
}

/// The grain window drawn small: its curve, with the name and the size.
fn window_view(dev: &Dev) -> AnyElement {
    let (kind, shape) = (dev.choice(p::WINDOW), dev.plain(p::WIN_SHAPE));
    let enabled = dev.enabled;
    let name = dev.info(p::WINDOW).and_then(|i| i.choices.get(kind).copied()).unwrap_or("Hann");
    let text = format!("{name} · {:.0}%", shape);
    let surface = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let pal = p();
            let colour = if enabled { pal.accent } else { pal.muted };
            let r = Rect::of(b);
            let inner = Rect { x: r.x + 4., y: r.y + 4., w: r.w - 8., h: r.h - 22. };
            kit::hline(window, inner, inner.bottom(), pal.grid_bar);
            kit::vline(window, inner, inner.px(0.5), pal.grid_beat);
            let pts = kit::sample_curve(inner, 48, |u| 1. - window_value(kind, shape, u));
            kit::fill_to(window, &pts, inner.bottom(), with_alpha(colour, 0.16));
            kit::line(window, &pts, 1.5, colour);
            kit::txt(window, cx, &text, r.px(0.5), r.bottom() - 15., TEXT_SM, pal.muted, TextAlign::Center);
        },
    )
    .size_full();
    div().flex_none().w(px(116.)).h(px(76.)).rounded(px(r_sm())).bg(p().well).overflow_hidden().child(surface).into_any_element()
}

// ───────────────────────────── The stage ─────────────────────────────

/// What the stage draws, copied out of the device so the painter owns it.
struct Stage {
    view: Option<View>,
    enabled: bool,
    frozen: bool,
    source: usize,
    pos: f32,
    spray: f32,
    depth: f32,
    scan: f32,
    spread: f32,
    size_ms: f32,
    compact: bool,
}

fn stage(ws: &mut Workspace, dev: &Dev, view: Option<View>, compact: bool, cx: &mut Context<Workspace>) -> gpui::Stateful<Div> {
    let secs = view.as_ref().map_or(0., |v| v.secs);
    let s = Stage {
        frozen: dev.on(p::FREEZE),
        source: dev.choice(p::SOURCE),
        pos: dev.plain(p::POSITION) / 100.,
        spray: dev.plain(p::SPRAY) / 100.,
        depth: dev.plain(p::SCAN_DEPTH) / 100.,
        scan: dev.plain(p::SCAN_SPEED),
        spread: dev.plain(p::PAN_SPREAD) / 100.,
        size_ms: dev.plain(p::SIZE),
        enabled: dev.enabled,
        view,
        compact,
    };
    let g = grain_share(s.size_ms, secs);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    // Each handle's place is a plot fraction; a drag moves its parameter by
    // the distance over the whole plot (see `kit::ax`).
    let wide = 1. / (1. - g).max(0.1);
    let handles = vec![
        Handle { fx: centre(s.pos, g), fy: 0.5, x: ax(p::POSITION, wide), index: 0, reset: vec![default(p::POSITION)], ..Default::default() },
        Handle { fx: centre((s.pos + s.spray / 2.).min(1.), g), fy: 0.14, x: ax(p::SPRAY, 2. * wide), index: 1, reset: vec![default(p::SPRAY)], ..Default::default() },
        Handle { fx: centre((s.pos + s.depth / 2.).min(1.), g), fy: 0.9, x: ax(p::SCAN_DEPTH, 2. * wide), index: 2, reset: vec![default(p::SCAN_DEPTH)], ..Default::default() },
        Handle { fx: 0.97, fy: pan_y(-s.spread), y: ax(p::PAN_SPREAD, -1. / PAN_REACH), index: 3, reset: vec![default(p::PAN_SPREAD)], ..Default::default() },
    ];
    kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| paint_stage(&s, g, r, window, cx))
}

fn paint_stage(s: &Stage, g: f32, r: Rect, window: &mut gpui::Window, cx: &mut gpui::App) {
    let pal = p();
    let colour = if s.enabled { pal.accent } else { pal.muted };
    let ice = pal.accent2;
    let mid = r.py(0.5);
    let x_of = |pos: f32| r.px(centre(pos, g));
    // The time ruler (or percent while the length is not known) and the middle line.
    let secs = s.view.as_ref().map_or(0., |v| v.secs);
    for k in 0..=10 {
        let f = k as f32 / 10.;
        if k > 0 && k < 10 {
            kit::vline(window, r, r.px(f), if k == 5 { pal.grid_bar } else { pal.grid_beat });
        }
        if !s.compact && (k % 2 == 0) {
            let text = if secs > 0. { format!("{:.1}", secs * f) } else { format!("{:.0}%", f * 100.) };
            let align = if k == 0 { TextAlign::Left } else if k == 10 { TextAlign::Right } else { TextAlign::Center };
            let dx = if k == 0 { 6. } else if k == 10 { -6. } else { 0. };
            kit::txt(window, cx, &text, r.px(f) + dx, r.bottom() - 16., TEXT_SM, pal.faint, align);
        }
    }
    kit::hline(window, r, mid, pal.grid_bar);
    // The waveform, mirrored about the middle.
    let amp = r.h * 0.36;
    match s.view.as_ref().filter(|v| v.wave.iter().any(|w| *w > 0.001)) {
        Some(v) => {
            let n = v.wave.len();
            let at = |i: usize, sign: f32| {
                let x = r.px((i as f32 + 0.5) / n as f32);
                gpui::point(gpui::px(x), gpui::px(mid - sign * v.wave[i].min(1.) * amp))
            };
            let mut poly: Vec<gpui::Point<gpui::Pixels>> = (0..n).map(|i| at(i, 1.)).collect();
            poly.extend((0..n).rev().map(|i| at(i, -1.)));
            paint::polygon(window, &poly, with_alpha(ice, 0.3));
            let top: Vec<gpui::Point<gpui::Pixels>> = (0..n).map(|i| at(i, 1.)).collect();
            let bottom: Vec<gpui::Point<gpui::Pixels>> = (0..n).map(|i| at(i, -1.)).collect();
            kit::line(window, &top, 1., with_alpha(ice, 0.7));
            kit::line(window, &bottom, 1., with_alpha(ice, 0.7));
        }
        None => {
            let text = if s.view.is_none() { "play a note: the source and its grains show here" } else if s.source == SOURCE_LIVE { "the live source is empty: feed the sidechain input" } else { "the source is empty" };
            if !s.compact {
                kit::txt(window, cx, text, r.px(0.5), mid - 24., TEXT_SM, pal.faint, TextAlign::Center);
            }
        }
    }
    // Where the pan sends the grains: the reach either way, with L and R at the edge.
    for sign in [1., -1.] {
        let y = r.py(0.5 - sign * s.spread * PAN_REACH);
        kit::hline(window, r, y, with_alpha(pal.text, 0.1));
    }
    if !s.compact {
        kit::txt(window, cx, "L", r.right() - 6., r.py(0.5 - PAN_REACH) + 4., TEXT_SM, pal.faint, TextAlign::Right);
        kit::txt(window, cx, "R", r.right() - 6., r.py(0.5 + PAN_REACH) + 2., TEXT_SM, pal.faint, TextAlign::Right);
    }
    // The spray: the range the grains are drawn from around the position.
    let (a, b) = (x_of((s.pos - s.spray / 2.).max(0.)), x_of((s.pos + s.spray / 2.).min(1.)));
    kit::bar(window, a, r.y, (b - a).max(1.), r.h, 0., with_alpha(colour, 0.09));
    kit::vline(window, r, a, with_alpha(colour, 0.45));
    kit::vline(window, r, b, with_alpha(colour, 0.45));
    // The scan: the LFO's reach along the bottom, and the way the scan runs.
    let (c0, c1) = (x_of((s.pos - s.depth / 2.).max(0.)), x_of((s.pos + s.depth / 2.).min(1.)));
    let scan_y = r.py(0.9);
    if s.depth > 0. {
        kit::bar(window, c0, scan_y - 2., (c1 - c0).max(2.), 4., 2., with_alpha(pal.accent2, 0.8));
    }
    if s.scan != 0. {
        let x = r.px(centre(s.pos, g));
        let dir = s.scan.signum();
        let len = (s.scan.abs() / 200.).max(0.04) * r.w * 0.35;
        let y = scan_y - 12.;
        kit::line(window, &[gpui::point(gpui::px(x), gpui::px(y)), gpui::point(gpui::px(x + dir * len), gpui::px(y))], 1.5, with_alpha(pal.accent2, 0.9));
        kit::line(window, &[gpui::point(gpui::px(x + dir * len - dir * 5.), gpui::px(y - 3.)), gpui::point(gpui::px(x + dir * len), gpui::px(y))], 1.5, with_alpha(pal.accent2, 0.9));
        kit::line(window, &[gpui::point(gpui::px(x + dir * len - dir * 5.), gpui::px(y + 3.)), gpui::point(gpui::px(x + dir * len), gpui::px(y))], 1.5, with_alpha(pal.accent2, 0.9));
        if !s.compact {
            let (tx, align) = if dir > 0. { (x + dir * len + 8., TextAlign::Left) } else { (x + dir * len - 8., TextAlign::Right) };
            kit::txt(window, cx, &format!("scan {:+.0} %/s", s.scan), tx, y - 6., TEXT_SM, pal.accent2, align);
        }
    }
    // The grains: as long as they are, fading with age, high or low by pan.
    if let Some(v) = &s.view {
        let bar_h = if s.compact { 6. } else { 10. };
        for gr in &v.grains {
            let w = (g * r.w).max(4.);
            let x = x_of(gr.pos) - w / 2.;
            let life = (1. - gr.age).clamp(0., 1.);
            kit::bar(window, x, r.py(pan_y(gr.pan)) - bar_h / 2., w, bar_h, 3., with_alpha(colour, 0.12 + 0.8 * life));
        }
        // The last grain started.
        if !v.grains.is_empty() {
            let x = x_of(v.last);
            kit::bar(window, x - 1., r.y, 2., 8., 1., with_alpha(pal.text, 0.8));
        }
    }
    // The position line over everything.
    let px_pos = x_of(s.pos);
    kit::vline(window, r, px_pos, with_alpha(pal.text, 0.7));
    for (fx, fy, size) in [(centre(s.pos, g), 0.5, 7.), (centre((s.pos + s.spray / 2.).min(1.), g), 0.14, 5.), (centre((s.pos + s.depth / 2.).min(1.), g), 0.9, 5.)] {
        kit::dot(window, r.pt(fx, fy), size, if s.enabled { pal.accent } else { pal.muted }, Some(pal.text));
    }
    kit::dot(window, r.pt(0.97, pan_y(-s.spread)), 5., pal.accent2, Some(pal.text));
    // The state, in the corners.
    if let Some(v) = &s.view
        && !s.compact
    {
        if s.source == SOURCE_LIVE {
            kit::bar(window, r.x, r.y, r.w, 4., 0., content(0.08));
            kit::bar(window, r.x, r.y, r.w * v.fill, 4., 0., with_alpha(ice, 0.9));
            kit::txt(window, cx, &format!("live buffer {:.0}% · {}", v.fill * 100., fmt_secs(v.secs)), r.x + 8., r.y + 10., TEXT_SM, pal.muted, TextAlign::Left);
        }
        let right = format!("{} voices · {} grains", v.voices, v.grains.len());
        kit::txt(window, cx, &right, r.right() - 8., r.y + 10., TEXT_SM, pal.muted, TextAlign::Right);
    }
    if s.frozen {
        kit::bar(window, r.x, r.y + r.h - 3., r.w, 3., 0., with_alpha(ice, 0.9));
        if !s.compact {
            kit::txt(window, cx, "FROZEN", r.x + 8., r.y + if s.source == SOURCE_LIVE { 28. } else { 10. }, TEXT_SM, ice, TextAlign::Left);
        }
    }
}

// ───────────────────────────── The editor ─────────────────────────────

/// The source: which one, its extra control, and Freeze.
fn source_card(dev: &Dev, view: &Option<View>, cx: &mut Context<Workspace>) -> Div {
    let src = dev.choice(p::SOURCE);
    let pal = p();
    let mut body = vec![chips_named(dev, p::SOURCE, false, cx)];
    match src {
        SOURCE_BUILTIN => body.push(kit::pick(dev, p::BUILTIN, cx)),
        SOURCE_LIVE => body.push(knob(dev, p::LIVE_LEN, cx)),
        _ => {}
    }
    body.push(freeze_button(dev, src == SOURCE_LIVE, cx));
    let note = match view {
        Some(v) if v.source < SOURCES.len() && v.secs > 0. => format!("{} · {}", SOURCES[v.source], fmt_secs(v.secs)),
        _ => SOURCES[src.min(SOURCES.len() - 1)].to_owned(),
    };
    div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP3))
        .p(px(SP3))
        .rounded(px(r_md()))
        .bg(content(0.03))
        .child(label("SOURCE", TEXT_SM, pal.muted).font_weight(FontWeight::SEMIBOLD))
        .children(body)
        .child(div().flex_1())
        .child(mono(note, TEXT_SM, pal.faint))
}

fn more_open(id: DeviceId) -> bool {
    MORE.with(|m| m.borrow().contains(&id))
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let view = read(&dev.meter);
    let in_window = kit::in_window();
    if !in_window {
        return card(ws, dev, view, cx);
    }
    let open = more_open(dev.id);
    let device = dev.id;
    let sync = dev.choice(p::SYNC) != 0;

    // GRAINS: the stream's size, rate and shape, with the window drawn beside them.
    let mut grains = vec![knob(dev, p::SIZE, cx)];
    if sync {
        grains.push(kit::pick(dev, p::SYNC, cx));
        grains.push(knob(dev, p::SYNC_MULT, cx));
    } else {
        grains.push(knob(dev, p::DENSITY, cx));
    }
    grains.extend([knob(dev, p::JITTER, cx), knob(dev, p::WIN_SHAPE, cx)]);
    grains.push(div().flex_none().flex().flex_col().gap(px(SP1)).child(window_view(dev)).child(kit::pick(dev, p::WINDOW, cx)).into_any_element());
    let tone = vec![knob(dev, p::PITCH, cx), knob(dev, p::PITCH_SPREAD, cx), knob(dev, p::PAN_SPREAD, cx)];
    let out = vec![knob(dev, p::VOLUME, cx), knob(dev, p::WIDTH, cx)];
    let toggle = div()
        .id(("gr-more", dev.key(0)))
        .flex_none()
        .h(px(28.))
        .px(px(SP3))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if open { accent_alpha(0.18) } else { content(0.06) })
        .hover(|s| s.bg(content(0.12)))
        .child(label(if open { "Less ▴" } else { "More ▾" }, TEXT_SM, if open { p().accent } else { p().text }).font_weight(FontWeight::MEDIUM))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |_, _, _, cx| {
            MORE.with(|m| {
                let mut m = m.borrow_mut();
                if !m.remove(&device) {
                    m.insert(device);
                }
            });
            cx.notify();
        }));
    let main_row = div().flex_none().flex().flex_wrap().items_stretch().justify_between().gap(px(SP2)).child(group("Grains", grains)).child(group("Tone", tone)).child(group("Output", out)).child(
        div().flex_none().flex().items_end().child(toggle),
    );

    // The rest, in a drawer: the secondary settings of each group, the envelope and the filter.
    let drawer = open.then(|| {
        let more_grains = vec![knob(dev, p::SIZE_RAND, cx), ctl(dev, p::SYNC, cx), ctl(dev, p::DIRECTION, cx)];
        let more_tone = vec![knob(dev, p::KEY_TRACK, cx), ctl(dev, p::QUANT, cx), ctl(dev, p::SCALE, cx), ctl(dev, p::ROOT, cx), knob(dev, p::ROOT_KEY, cx), knob(dev, p::SEED, cx)];
        let env: Vec<AnyElement> = [p::ATTACK, p::DECAY, p::SUSTAIN, p::RELEASE, p::VEL_SENS].into_iter().map(|id| knob(dev, id, cx)).collect();
        let mut filter = vec![ctl(dev, p::FILTER_TYPE, cx)];
        if dev.choice(p::FILTER_TYPE) != 0 {
            filter.extend([p::CUTOFF, p::RESONANCE, p::FILTER_ENV, p::FILTER_KEY].into_iter().map(|id| knob(dev, id, cx)));
        }
        let more_out = vec![knob(dev, p::POLYPHONY, cx), knob(dev, p::BEND_RANGE, cx)];
        div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_stretch()
            .gap(px(SP2))
            .child(group("Envelope", env))
            .child(group("Filter", filter))
            .child(group("Grain stream", more_grains))
            .child(group("Pitch", more_tone))
            .child(group("Voices", more_out))
    });

    let stage = stage(ws, dev, view.clone(), false, cx);
    let frame = div().flex().flex_col().flex_grow(1.).flex_basis(px(STAGE_MIN)).min_h(px(STAGE_MIN)).child(stage.flex_1().min_h(px(0.)));
    kit::panel(760.)
        .child(
            div().id(("granular-body", dev.key(0))).flex_1().min_h(px(0.)).overflow_y_scroll().child(
                div().min_h_full().flex().flex_col().gap(px(SP2)).child(source_card(dev, &view, cx)).child(frame).child(main_row).children(drawer),
            ),
        )
        .into_any_element()
}

/// The compact card in the Device View: the stage, and the few things that matter most.
fn card(ws: &mut Workspace, dev: &Dev, view: Option<View>, cx: &mut Context<Workspace>) -> AnyElement {
    let w = if dev.big { 520. } else { 400. };
    let live = dev.choice(p::SOURCE) == SOURCE_LIVE;
    let stage = stage(ws, dev, view, true, cx);
    kit::panel(w)
        .child(div().flex_none().h(px(if dev.big { 132. } else { 96. })).child(stage))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .child(chips(dev, p::SOURCE, cx))
                .child(freeze_button(dev, live, cx)),
        )
        .child(div().flex_none().flex().flex_wrap().gap(px(SP1)).children([p::SIZE, p::DENSITY, p::PITCH, p::VOLUME].into_iter().map(|id| knob(dev, id, cx))))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    /// A meter as the device writes it: three live grains and a ramp for a waveform.
    fn meter() -> Vec<f32> {
        let mut m = vec![0.; METER_LEN];
        m[..8].copy_from_slice(&[2., 3., 0.4, 0.25, 1., 1., 4., 0.5]);
        for i in 0..64 {
            m[WAVE_AT + i] = i as f32 / 63.;
        }
        for k in 0..GRAINS {
            m[GRAINS_AT + 3 * k] = -1.;
        }
        for (k, (pos, age, pan)) in [(0.1, 0.0, -1.0), (0.5, 0.5, 0.0), (0.9, 0.9, 1.0)].into_iter().enumerate() {
            m[GRAINS_AT + 3 * k..GRAINS_AT + 3 * k + 3].copy_from_slice(&[pos, age, pan]);
        }
        m
    }

    #[test]
    fn the_meter_is_read_into_live_grains_and_state() {
        let v = read(&meter()).expect("a full meter");
        assert_eq!((v.voices, v.grains.len(), v.frozen, v.source), (2, 3, true, 1));
        assert_eq!((v.secs, v.fill, v.last), (4., 0.5, 0.25));
        assert_eq!(v.grains[2], Grain { pos: 0.9, age: 0.9, pan: 1. });
        assert_eq!(v.wave.len(), 64);
        assert_eq!(v.wave[63], 1.);
        // Free grains (-1) are left out, and nothing is read until the meter is whole.
        assert!(read(&meter()[..100]).is_none() && read(&[]).is_none());
        assert_eq!(METER_LEN, 264);
    }

    #[test]
    fn a_grain_is_centred_where_the_device_reads_it() {
        // Position 0 puts the first grain at the start, 100 % the last one that fits at the end.
        assert_eq!(centre(0., 0.2), 0.1);
        assert!((centre(1., 0.2) - 0.9).abs() < 1e-6);
        assert_eq!(centre(0.5, 0.), 0.5);
        // A grain covers its length over the source, and an unknown length covers nothing.
        assert!((grain_share(500., 2.) - 0.25).abs() < 1e-6);
        assert_eq!(grain_share(100., 0.), 0.);
        assert_eq!(grain_share(5_000., 1.), 0.9);
    }

    #[test]
    fn the_pan_puts_left_up_and_right_down_inside_the_stage() {
        assert!(pan_y(-1.) < pan_y(0.) && pan_y(0.) < pan_y(1.));
        assert_eq!(pan_y(0.), 0.5);
        assert!(pan_y(-5.) >= 0.5 - PAN_REACH - 1e-6 && pan_y(5.) <= 0.5 + PAN_REACH + 1e-6);
    }

    #[test]
    fn every_window_is_zero_at_the_ends_and_its_peak_follows_the_shape() {
        for kind in 0..6 {
            for shape in [0., 50., 100.] {
                assert!(window_value(kind, shape, 0.).abs() < 1e-5 && window_value(kind, shape, 1.).abs() < 1e-5, "kind {kind} shape {shape}");
                let top = (0..=200).map(|i| window_value(kind, shape, i as f32 / 200.)).fold(0f32, f32::max);
                assert!((0.99..=1.0001).contains(&top), "kind {kind} shape {shape}: {top}");
            }
        }
        // Shape 100 % peaks early (a percussive grain), 0 % late (a swell), 50 % in the middle.
        let at_max = |shape: f32| (0..=200).map(|i| i as f32 / 200.).max_by(|a, b| window_value(0, shape, *a).total_cmp(&window_value(0, shape, *b))).unwrap();
        assert!(at_max(100.) < 0.2 && at_max(0.) > 0.8 && (at_max(50.) - 0.5).abs() < 0.02);
    }

    #[test]
    fn the_editor_reads_the_real_parameters() {
        let d = dev("instrument.granular", &[(p::SOURCE, 1.), (p::SIZE, 250.), (p::POSITION, 80.), (p::FREEZE, 1.)]);
        assert_eq!((d.choice(p::SOURCE), d.plain(p::POSITION).round(), d.on(p::FREEZE)), (1, 80., true));
        assert!((d.plain(p::SIZE) - 250.).abs() < 1.);
        for id in [p::SOURCE, p::WINDOW, p::SCALE, p::ROOT, p::FILTER_TYPE, p::QUANT, p::DIRECTION, p::SYNC, p::BUILTIN] {
            assert!(!d.info(id).expect("a parameter").choices.is_empty(), "{id} is a choice");
        }
        assert_eq!(d.info(p::WINDOW).unwrap().choices.len(), 6);
    }

    #[test]
    fn the_drawer_is_remembered_per_device() {
        let (a, b) = (DeviceId(901), DeviceId(902));
        assert!(!more_open(a));
        MORE.with(|m| m.borrow_mut().insert(a));
        assert!(more_open(a) && !more_open(b));
        MORE.with(|m| m.borrow_mut().remove(&a));
        assert!(!more_open(a));
    }
}
