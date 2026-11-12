//! Reusable controls: icon buttons, toggles, segmented controls, knobs,
//! faders, meters, menus, tooltips and a minimal text field. They only draw
//! and report input; the caller decides what an interaction does (always a
//! `Command` through `AppState`).

use crate::bridge::MeterLevel;
use crate::icons::{Icon, icon};
use crate::motion;
use crate::paint;
use crate::theme::{self, *};
use crate::theme::{FaderStyle, KnobStyle, MeterStyle};
use gpui::{
    AnyView, App, Context, Div, ElementId, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Window, canvas, div, point, prelude::FluentBuilder, px,
};
use gpui::{AnimationExt, AppContext as _};
use std::rc::Rc;

// ───────────────────────────── Text ─────────────────────────────

pub fn label(text: impl Into<SharedString>, size: f32, color: Hsla) -> Div {
    let size = fs(size);
    div().text_size(px(size)).line_height(px((size * 1.35).round())).text_color(color).whitespace_nowrap().child(text.into())
}

pub fn mono(text: impl Into<SharedString>, size: f32, color: Hsla) -> Div {
    label(text, size, color).font_family(FONT_MONO)
}

/// A small rounded badge (key, BPM, format).
pub fn badge(text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .flex_none()
        .h(px(16.))
        .px(px(SP1 + 1.))
        .rounded(px(r_sm()))
        .bg(with_alpha(color, 0.14))
        .flex()
        .items_center()
        .child(label(text, TEXT_XS, color).font_weight(FontWeight::MEDIUM))
}

pub fn divider_v(h: f32) -> Div {
    div().w(px(1.)).h(px(h)).flex_none().bg(border())
}

pub fn divider_h() -> Div {
    div().h(px(1.)).w_full().flex_none().bg(border())
}

// ───────────────────────────── Tooltip ─────────────────────────────

pub struct Tooltip {
    text: SharedString,
    hint: Option<SharedString>,
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        motion::fade_in(
            "tooltip",
            div()
                .px(px(SP2))
                .py(px(SP1))
                .rounded(px(r_md()))
                .bg(pal.raised)
                .border_1()
                .border_color(border_strong())
                .shadow_md()
                .flex()
                .items_center()
                .gap(px(SP2))
                .font_family(font_ui())
                .child(label(self.text.clone(), TEXT_SM, pal.text))
                .when_some(self.hint.clone(), |d, h| {
                    d.child(
                        div().px(px(SP1)).rounded(px(r_sm())).bg(content(0.08)).child(mono(h, TEXT_XS, pal.muted)),
                    )
                }),
        )
    }
}

/// A tooltip builder for `.tooltip(...)`.
pub fn tip(text: &'static str, hint: Option<&'static str>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| Tooltip { text: text.into(), hint: hint.map(Into::into) }).into()
}

/// Tooltip with owned text.
pub fn tip_owned(text: String) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| Tooltip { text: text.clone().into(), hint: None }).into()
}

// ───────────────────────────── Buttons ─────────────────────────────

/// A square icon button, `CONTROL_H` px, accent when active.
pub fn icon_button(id: impl Into<ElementId>, i: Icon, active: bool) -> Stateful<Div> {
    sized_icon_button(id, i, active, CONTROL_H, 16.)
}

pub fn sized_icon_button(id: impl Into<ElementId>, i: Icon, active: bool, side: f32, glyph: f32) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(side))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .when(active, |d| d.bg(accent_alpha(0.16)))
        .when(!active, |d| d.hover(|s| s.bg(hover())).active(|s| s.bg(pressed())))
        .child(icon(i, glyph, if active { pal.accent } else { pal.muted }))
}

/// A labelled button with an optional leading icon.
pub fn text_button(id: impl Into<ElementId>, text: impl Into<SharedString>, i: Option<Icon>, primary: bool) -> Stateful<Div> {
    let pal = p();
    let fg = if primary { pal.on_accent } else { pal.text };
    div()
        .id(id)
        .flex_none()
        .h(px(CONTROL_H))
        .px(px(SP3))
        .gap(px(SP2))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .bg(if primary { pal.accent } else { pal.raised })
        .border_1()
        .border_color(if primary { pal.accent } else { border() })
        .hover(|s| s.opacity(0.9))
        .active(|s| s.opacity(0.75))
        .when_some(i, |d, i| d.child(icon(i, 14., fg)))
        .child(label(text, TEXT_MD, fg).font_weight(FontWeight::MEDIUM))
}

/// A tiny square toggle: mute/solo/arm, 20 px.
pub fn chip(id: impl Into<ElementId>, text: &'static str, on: bool, on_color: Hsla) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if on { on_color } else { content(0.06) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
        .child(
            label(text, TEXT_XS, if on { theme::mix(on_color, with_alpha(pal.shadow, 1.), 0.78) } else { pal.muted })
                .font_weight(FontWeight::SEMIBOLD),
        )
}

/// An icon chip with the same footprint as [`chip`].
pub fn icon_chip(id: impl Into<ElementId>, i: Icon, on: bool, on_color: Hsla) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(if on { with_alpha(on_color, 0.2) } else { content(0.06) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
        .child(icon(i, 12., if on { on_color } else { pal.muted }))
}

// ───────────────────────────── Segmented control ─────────────────────────────

pub type OnSelect = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// Equal-width segments with a sliding indicator (spring, keeps velocity
/// when changed mid-flight). `items` are `(icon, label)`.
pub fn segmented(id: &'static str, items: &[(Option<Icon>, &'static str)], selected: usize, seg_w: f32, on_select: OnSelect) -> Div {
    let pal = p();
    let h = CONTROL_H;
    let inner = h - 4.;
    let indicator = div()
        .absolute()
        .top(px(2.))
        .w(px(seg_w))
        .h(px(inner))
        .rounded(px(r_md() - 1.))
        .bg(pal.raised)
        .border_1()
        .border_color(border_strong())
        .shadow_sm()
        .with_spring(
            ElementId::Name(format!("{id}-indicator").into()),
            motion::spring(motion::INDICATOR_SPRING, 2. + seg_w * selected as f32),
            |d, x| d.left(px(x)),
        );
    let mut row = div()
        .relative()
        .flex_none()
        .h(px(h))
        .w(px(seg_w * items.len() as f32 + 4.))
        .p(px(2.))
        .rounded(px(r_md() + 1.))
        .bg(content(0.05))
        .border_1()
        .border_color(border())
        .flex()
        .child(indicator);
    for (i, (ic, text)) in items.iter().enumerate() {
        let on = i == selected;
        let color = if on { pal.text } else { pal.muted };
        let cb = on_select.clone();
        row = row.child(
            div()
                .id(ElementId::Name(format!("{id}-{i}").into()))
                .relative()
                .w(px(seg_w))
                .h(px(inner))
                .flex()
                .items_center()
                .justify_center()
                .gap(px(SP1 + 2.))
                .cursor_pointer()
                .when_some(*ic, |d, ic| d.child(icon(ic, 14., if on { pal.accent } else { color })))
                .when(!text.is_empty(), |d| d.child(label(*text, TEXT_SM, color).font_weight(FontWeight::MEDIUM)))
                .on_click(move |_, window, cx| cb(i, window, cx)),
        );
    }
    row
}

// ───────────────────────────── Knob ─────────────────────────────

const KNOB_A0: f32 = std::f32::consts::PI * 0.75;
const KNOB_SPAN: f32 = std::f32::consts::PI * 1.5;

/// A rotary knob painted as an arc (style from the theme). The caller attaches the drag.
pub fn knob(id: impl Into<ElementId>, value: f32, bipolar: bool, side: f32, color: Hsla) -> Stateful<Div> {
    let value = value.clamp(0., 1.);
    let knob_style = theme::style().knob;
    div()
        .id(id)
        .flex_none()
        .size(px(side))
        .cursor_ns_resize()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let pal = p();
                    let c = bounds.center();
                    let r = side / 2. - 3.;
                    let (a, b) = if bipolar {
                        let mid = KNOB_A0 + KNOB_SPAN / 2.;
                        let v = KNOB_A0 + KNOB_SPAN * value;
                        (mid.min(v), mid.max(v))
                    } else {
                        (KNOB_A0, KNOB_A0 + KNOB_SPAN * value)
                    };
                    let ang = KNOB_A0 + KNOB_SPAN * value;
                    match knob_style {
                        KnobStyle::Arc => {
                            paint::arc(window, c, r, KNOB_A0, KNOB_A0 + KNOB_SPAN, 3., content(0.12));
                            paint::arc(window, c, r, a, b, 3., color);
                            let body = r - 4.;
                            window.paint_quad(paint::rrect(
                                gpui::Bounds::new(point(c.x - px(body), c.y - px(body)), gpui::size(px(body * 2.), px(body * 2.))),
                                body,
                                pal.raised,
                            ));
                            let p0 = point(c.x + px((body * 0.25) * ang.cos()), c.y + px((body * 0.25) * ang.sin()));
                            let p1 = point(c.x + px((body - 1.) * ang.cos()), c.y + px((body - 1.) * ang.sin()));
                            paint::polyline(window, &[p0, p1], 2., pal.text);
                        }
                        KnobStyle::Minimal => {
                            paint::arc(window, c, r - 1., KNOB_A0, KNOB_A0 + KNOB_SPAN, 1.5, content(0.16));
                            paint::arc(window, c, r - 1., a, b, 1.5, color);
                            let d = point(c.x + px((r - 1.) * ang.cos()), c.y + px((r - 1.) * ang.sin()));
                            window.paint_quad(paint::rrect(gpui::Bounds::new(point(d.x - px(3.5), d.y - px(3.5)), gpui::size(px(7.), px(7.))), 3.5, color));
                        }
                    }
                },
            )
            .size_full(),
        )
}

/// A labelled knob cell of fixed width so rows of knobs align.
pub fn knob_cell(knob: Stateful<Div>, name: &str, value_text: String, width: f32) -> Div {
    let pal = p();
    div()
        .flex_none()
        .w(px(width))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(label(paint::fit(name, width, TEXT_XS), TEXT_XS, pal.muted))
        .child(knob)
        .child(mono(paint::fit(&value_text, width, TEXT_XS), TEXT_XS, pal.text))
}

// ───────────────────────────── Fader & meter ─────────────────────────────

/// A vertical fader (position 0..=1) with the 0 dB mark. The caller attaches the drag.
pub fn fader(id: impl Into<ElementId>, pos: f32, height: f32, color: Hsla) -> Stateful<Div> {
    let pos = pos.clamp(0., 1.);
    let slim = theme::style().fader == FaderStyle::Slim;
    div().id(id).flex_none().w(px(24.)).h(px(height)).cursor_ns_resize().child(
        canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let pal = p();
                let x = b.center().x;
                let top = f32::from(b.origin.y) + 6.;
                let len = f32::from(b.size.height) - 12.;
                let tw = if slim { 2. } else { 4. };
                let track = gpui::Bounds::new(point(x - px(tw / 2.), px(top)), gpui::size(px(tw), px(len)));
                window.paint_quad(paint::rrect(track, tw / 2., content(0.10)));
                let y = top + len * (1. - pos);
                let filled = gpui::Bounds::new(point(x - px(tw / 2.), px(y)), gpui::size(px(tw), px(top + len - y)));
                window.paint_quad(paint::rrect(filled, tw / 2., with_alpha(color, 0.7)));
                let unity = top + len * 0.25;
                for dx in [2., f32::from(b.size.width) - 6.] {
                    window.paint_quad(paint::fill_rect(gpui::Bounds::new(point(b.origin.x + px(dx), px(unity)), gpui::size(px(4.), px(1.))), content(0.35)));
                }
                if slim {
                    window.paint_quad(paint::rrect_border(gpui::Bounds::new(point(x - px(6.), px(y - 6.)), gpui::size(px(12.), px(12.))), 6., pal.raised, color, 2.));
                } else {
                    let thumb = gpui::Bounds::new(point(x - px(10.), px(y - 5.)), gpui::size(px(20.), px(10.)));
                    window.paint_quad(paint::rrect_border(thumb, 3., pal.raised, border_strong(), 1.));
                    window.paint_quad(paint::fill_rect(gpui::Bounds::new(point(x - px(6.), px(y - 0.5)), gpui::size(px(12.), px(1.))), pal.text));
                }
            },
        )
        .size_full(),
    )
}

/// A stereo peak meter in the theme's meter style. Pure paint, no elements per segment.
pub fn meter(level: MeterLevel, w: f32, h: f32) -> impl IntoElement {
    let meter_style = theme::style().meter;
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let pal = p();
            let gap = 2.;
            let horizontal = f32::from(b.size.width) > f32::from(b.size.height) * 2.;
            for (i, v) in [level.left, level.right].into_iter().enumerate() {
                // Wide meters stack the two channels; tall ones sit side by side.
                let (x, y, bw, bh) = if horizontal {
                    let bh = (f32::from(b.size.height) - gap) / 2.;
                    (f32::from(b.origin.x), f32::from(b.origin.y) + i as f32 * (bh + gap), f32::from(b.size.width), bh)
                } else {
                    let bw = (f32::from(b.size.width) - gap) / 2.;
                    (f32::from(b.origin.x) + i as f32 * (bw + gap), f32::from(b.origin.y), bw, f32::from(b.size.height))
                };
                window.paint_quad(paint::rrect(paint::rect(x, y, bw, bh), 1.5, content(0.07)));
                let v = v.clamp(0., 1.);
                if v <= 0.001 {
                    continue;
                }
                let solid = [(0., 1., pal.accent)];
                let zoned = [(0., 0.72, pal.meter_low), (0.72, 0.92, pal.meter_mid), (0.92, 1., pal.meter_high)];
                let zones: &[(f32, f32, Hsla)] = if meter_style == MeterStyle::Solid { &solid } else { &zoned };
                for &(lo, hi, color) in zones {
                    if v <= lo {
                        break;
                    }
                    let top = v.min(hi);
                    let r = if horizontal {
                        paint::rect(x + bw * lo, y, bw * (top - lo), bh)
                    } else {
                        paint::rect(x, y + bh * (1. - top), bw, bh * (top - lo))
                    };
                    window.paint_quad(paint::rrect(r, 1., color));
                }
                if meter_style == MeterStyle::Segmented {
                    // LED gaps every 4 px across the lit part.
                    let span = if horizontal { bw } else { bh };
                    let mut s = 3.;
                    while s < span * v {
                        let r = if horizontal { paint::rect(x + s, y, 1., bh) } else { paint::rect(x, y + bh - s, bw, 1.) };
                        window.paint_quad(paint::fill_rect(r, pal.well));
                        s += 4.;
                    }
                }
            }
        },
    )
    .w(px(w))
    .h(px(h))
    .flex_none()
}

/// A horizontal level bar (CPU, progress).
pub fn hbar(value: f32, w: f32, h: f32, color: Hsla) -> impl IntoElement {
    let v = value.clamp(0., 1.);
    div()
        .flex_none()
        .w(px(w))
        .h(px(h))
        .rounded(px(h / 2.))
        .bg(content(0.08))
        .child(div().h_full().w(px((w * v).max(if v > 0. { h } else { 0. }))).rounded(px(h / 2.)).bg(color))
}

// ───────────────────────────── Form controls ─────────────────────────────

/// A pill switch whose knob glides on a spring.
pub fn switch(id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    let pal = p();
    let id: ElementId = id.into();
    let knob_id = ElementId::Name(format!("{id:?}-knob").into());
    div()
        .id(id)
        .flex_none()
        .relative()
        .w(px(32.))
        .h(px(18.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { pal.accent } else { content(0.14) })
        .child(
            div()
                .absolute()
                .top(px(2.))
                .size(px(14.))
                .rounded_full()
                .bg(if on { pal.on_accent } else { pal.text })
                .with_spring(knob_id, motion::spring(motion::INDICATOR_SPRING, if on { 16. } else { 2. }), |d, x| d.left(px(x))),
        )
}

/// A labelled row: fixed-width label column so every form lines up.
pub fn form_row(title: impl Into<SharedString>, hint: Option<SharedString>, control: impl IntoElement) -> Div {
    let pal = p();
    div()
        .min_h(px(36.))
        .py(px(SP1))
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(
            div()
                .w(px(152.))
                .flex_none()
                .flex()
                .flex_col()
                .child(label(title, TEXT_MD, pal.text))
                .when_some(hint, |d, h| d.child(label(h, TEXT_XS, pal.faint))),
        )
        .child(div().flex_1().min_w(px(0.)).flex().items_center().gap(px(SP2)).child(control))
}

fn setting_text(title: SharedString, hint: Option<SharedString>) -> Div {
    let pal = p();
    div()
        .flex_1()
        .min_w(px(0.))
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(label(title, TEXT_MD, pal.text))
        .when_some(hint, |d, h| {
            d.child(label(h, TEXT_XS, pal.faint).text_ellipsis())
        })
}

/// A settings row: text on the left, the control pinned to the right edge.
/// Rows are separated by hairlines so the eye can follow each label across.
pub fn setting_row(
    title: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
) -> Div {
    div()
        .min_h(px(48.))
        .py(px(SP2))
        .flex()
        .items_center()
        .gap(px(SP6))
        .border_b_1()
        .border_color(border())
        .child(setting_text(title.into(), hint))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_end()
                .gap(px(SP2))
                .child(control),
        )
}

/// A settings row whose control needs the full width (lists, wrapping chips):
/// text on top, the control below it.
pub fn setting_block(
    title: impl Into<SharedString>,
    hint: Option<SharedString>,
    control: impl IntoElement,
) -> Div {
    div()
        .py(px(SP2))
        .flex()
        .flex_col()
        .gap(px(SP2))
        .border_b_1()
        .border_color(border())
        .child(setting_text(title.into(), hint))
        .child(div().w_full().child(control))
}

pub fn section_title(text: impl Into<SharedString>) -> Div {
    div().pt(px(SP4)).pb(px(SP1)).child(label(text, TEXT_XS, p().faint).font_weight(FontWeight::SEMIBOLD))
}

/// `[-] value [+]` for numeric settings.
pub fn stepper(id: &'static str, value: String) -> (Div, Stateful<Div>, Stateful<Div>) {
    let pal = p();
    let dec = sized_icon_button(ElementId::Name(format!("{id}-dec").into()), Icon::Minus, false, 24., 12.);
    let inc = sized_icon_button(ElementId::Name(format!("{id}-inc").into()), Icon::Add, false, 24., 12.);
    let shell = div()
        .flex_none()
        .h(px(CONTROL_H))
        .px(px(2.))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .bg(pal.well)
        .border_1()
        .border_color(border());
    (shell.child(mono(value, TEXT_SM, pal.text).w(px(64.)).flex().justify_center()), dec, inc)
}

/// Assemble a stepper after the caller wired its buttons.
pub fn stepper_el(parts: (Div, Stateful<Div>, Stateful<Div>)) -> Div {
    let (shell, dec, inc) = parts;
    div().flex().items_center().child(dec).child(shell).child(inc)
}

/// A checkbox row (label + optional colour dot).
pub fn checkbox(id: impl Into<ElementId>, on: bool, text: impl Into<SharedString>, dot: Option<Hsla>) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .h(px(ROW_H))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .hover(|s| s.bg(hover()))
        .child(
            div()
                .size(px(16.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(r_sm()))
                .border_1()
                .border_color(if on { pal.accent } else { border_strong() })
                .bg(if on { pal.accent } else { gpui::transparent_black() })
                .when(on, |d| d.child(icon(Icon::Check, 12., pal.on_accent))),
        )
        .when_some(dot, |d, c| d.child(div().size(px(8.)).rounded_full().bg(c)))
        .child(label(text, TEXT_MD, pal.text))
}

/// A colour swatch (a checkerboard shows through transparent colours).
pub fn swatch(id: impl Into<ElementId>, color: Hsla, side: f32, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(side))
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(content(0.12))
        .border_1()
        .border_color(if selected { accent() } else { border_strong() })
        .child(div().size_full().rounded(px(r_sm())).bg(color))
}

/// HSV ↔ colour, for the picker.
pub fn hsv_to_hsla(h: f32, s: f32, v: f32) -> Hsla {
    let c = v * s;
    let hp = (h.rem_euclid(1.)) * 6.;
    let x = c * (1. - (hp % 2. - 1.).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    let m = v - c;
    gpui::Rgba { r: r + m, g: g + m, b: b + m, a: 1. }.into()
}

pub fn hsla_to_hsv(c: Hsla) -> (f32, f32, f32) {
    let r = c.to_rgb();
    let max = r.r.max(r.g).max(r.b);
    let min = r.r.min(r.g).min(r.b);
    let d = max - min;
    let h = if d < 1e-6 {
        0.
    } else if max == r.r {
        ((r.g - r.b) / d).rem_euclid(6.) / 6.
    } else if max == r.g {
        ((r.b - r.r) / d + 2.) / 6.
    } else {
        ((r.r - r.g) / d + 4.) / 6.
    };
    (h, if max <= 0. { 0. } else { d / max }, max)
}

/// The saturation/value square of a colour picker for hue `h` (pure paint).
pub fn sv_square(h: f32, s: f32, v: f32, w: f32, hh: f32, bounds_out: Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>) -> impl IntoElement {
    canvas(
        move |b, _, _| bounds_out.set(b),
        move |b, _, window, _| {
            let steps = 24;
            let cw = f32::from(b.size.width) / steps as f32;
            let ch = f32::from(b.size.height) / steps as f32;
            for i in 0..steps {
                for j in 0..steps {
                    let c = hsv_to_hsla(h, i as f32 / (steps - 1) as f32, 1. - j as f32 / (steps - 1) as f32);
                    window.paint_quad(paint::fill_rect(paint::at(b.origin, i as f32 * cw, j as f32 * ch, cw + 0.5, ch + 0.5), c));
                }
            }
            let x = s * f32::from(b.size.width);
            let y = (1. - v) * f32::from(b.size.height);
            window.paint_quad(paint::rrect_border(paint::at(b.origin, x - 6., y - 6., 12., 12.), 6., gpui::transparent_black(), p().text, 2.));
        },
    )
    .w(px(w))
    .h(px(hh))
    .rounded(px(r_sm()))
}

/// The hue strip of a colour picker.
pub fn hue_strip(h: f32, w: f32, hh: f32, bounds_out: Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>) -> impl IntoElement {
    canvas(
        move |b, _, _| bounds_out.set(b),
        move |b, _, window, _| {
            let steps = 48;
            let cw = f32::from(b.size.width) / steps as f32;
            for i in 0..steps {
                window.paint_quad(paint::fill_rect(paint::at(b.origin, i as f32 * cw, 0., cw + 0.5, f32::from(b.size.height)), hsv_to_hsla(i as f32 / steps as f32, 0.9, 1.)));
            }
            let x = h * f32::from(b.size.width);
            window.paint_quad(paint::rrect_border(paint::at(b.origin, x - 3., -2., 6., f32::from(b.size.height) + 4.), 3., gpui::transparent_black(), p().text, 2.));
        },
    )
    .w(px(w))
    .h(px(hh))
}

// ───────────────────────────── Menus ─────────────────────────────

/// The card a popover or context menu draws in, rising in on open.
pub fn menu_card(id: impl Into<ElementId>, width: f32) -> Div {
    let pal = p();
    let _ = id;
    div()
        .w(px(width))
        .p(px(SP1))
        .rounded(px(r_lg()))
        .bg(pal.raised)
        .border_1()
        .border_color(border_strong())
        .shadow_lg()
        .flex()
        .flex_col()
        .font_family(font_ui())
}

pub fn menu_item(id: impl Into<ElementId>, i: Option<Icon>, text: impl Into<SharedString>, hint: Option<&'static str>, checked: bool) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .h(px(ROW_H))
        .px(px(SP2))
        .gap(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_md()))
        .cursor_pointer()
        .hover(|s| s.bg(hover()))
        .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().when_some(i, |d, i| {
            d.child(icon(i, 14., if checked { pal.accent } else { pal.muted }))
        }))
        .child(label(text, TEXT_MD, if checked { pal.accent } else { pal.text }).flex_1())
        .when_some(hint, |d, h| d.child(mono(h, TEXT_XS, pal.faint)))
        .when(checked, |d| d.child(icon(Icon::Check, 14., pal.accent)))
}

pub fn menu_header(text: impl Into<SharedString>) -> Div {
    div().h(px(24.)).px(px(SP2)).flex().items_center().child(label(text, TEXT_XS, p().faint).font_weight(FontWeight::MEDIUM))
}

// ───────────────────────────── Text field ─────────────────────────────

pub enum InputEvent {
    Changed(String),
    Submit(String),
}

/// A minimal single-line text field (search, chat). Handles typing,
/// backspace, Enter and Escape; no selection.
pub struct TextInput {
    text: String,
    placeholder: SharedString,
    leading: Option<Icon>,
    focus: FocusHandle,
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextInput {
    pub fn new(cx: &mut Context<Self>, placeholder: &'static str, leading: Option<Icon>) -> Self {
        Self { text: String::new(), placeholder: placeholder.into(), leading, focus: cx.focus_handle() }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.text = text.to_owned();
        cx.emit(InputEvent::Changed(self.text.clone()));
        cx.notify();
    }

    fn key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let mods = k.modifiers;
        match k.key.as_str() {
            "backspace" => {
                if mods.secondary() || mods.alt {
                    let trimmed = self.text.trim_end().rfind(' ').map_or(0, |i| i + 1);
                    self.text.truncate(trimmed);
                } else {
                    self.text.pop();
                }
                cx.emit(InputEvent::Changed(self.text.clone()));
            }
            "enter" => cx.emit(InputEvent::Submit(self.text.clone())),
            "escape" => window.blur(cx),
            _ if !mods.secondary() => {
                if let Some(ch) = k.key_char.as_ref().filter(|c| !c.chars().any(char::is_control)) {
                    self.text.push_str(ch);
                    cx.emit(InputEvent::Changed(self.text.clone()));
                } else {
                    return;
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = p();
        let focused = self.focus.is_focused(window);
        let empty = self.text.is_empty();
        div()
            .id("text-input")
            .key_context("TextInput")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus.focus(window, cx)))
            .h(px(CONTROL_H))
            .w_full()
            .px(px(SP2))
            .gap(px(SP2))
            .flex()
            .items_center()
            .rounded(px(r_md()))
            .bg(pal.well)
            .border_1()
            .border_color(if focused { accent_alpha(0.7) } else { border() })
            .cursor_text()
            .when_some(self.leading, |d, i| d.child(icon(i, 14., pal.faint)))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(label(if empty { self.placeholder.clone() } else { self.text.clone().into() }, TEXT_MD, if empty { pal.faint } else { pal.text }))
                    .when(focused, |d| d.child(div().w(px(1.)).h(px(14.)).ml(px(if empty { -1. } else { 1. })).bg(pal.accent))),
            )
            .when(!empty, |d| {
                d.child(
                    sized_icon_button("clear", Icon::X, false, 20., 12.)
                        .on_click(cx.listener(|this, _, _, cx| this.set_text("", cx))),
                )
            })
    }
}

