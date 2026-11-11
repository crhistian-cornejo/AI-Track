//! Reusable controls: icon buttons, toggles, segmented controls, knobs,
//! faders, meters, menus, tooltips and a minimal text field. They only draw
//! and report input; the caller decides what an interaction does (always a
//! `Command` through `AppState`).

use crate::bridge::MeterLevel;
use crate::icons::{Icon, icon};
use crate::motion;
use crate::paint;
use crate::theme::{self, *};
use gpui::{
    AnyView, App, Context, Div, ElementId, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement,
    Styled, TextAlign, Window, canvas, div, point, prelude::FluentBuilder, px,
};
use gpui::{AnimationExt, AppContext as _};
use std::rc::Rc;

// ───────────────────────────── Text ─────────────────────────────

pub fn label(text: impl Into<SharedString>, size: f32, color: Hsla) -> Div {
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
        .rounded(px(R_SM))
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
                .rounded(px(R_MD))
                .bg(pal.raised)
                .border_1()
                .border_color(border_strong())
                .shadow_md()
                .flex()
                .items_center()
                .gap(px(SP2))
                .font_family(FONT_UI)
                .child(label(self.text.clone(), TEXT_SM, pal.text))
                .when_some(self.hint.clone(), |d, h| {
                    d.child(
                        div().px(px(SP1)).rounded(px(R_SM)).bg(content(0.08)).child(mono(h, TEXT_XS, pal.muted)),
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
        .rounded(px(R_MD))
        .cursor_pointer()
        .when(active, |d| d.bg(accent_alpha(0.16)))
        .when(!active, |d| d.hover(|s| s.bg(hover())).active(|s| s.bg(pressed())))
        .child(icon(i, glyph, if active { pal.accent } else { pal.muted }))
}

/// A transport button: filled with `on_color` while on.
pub fn transport_button(id: impl Into<ElementId>, i: Icon, on: bool, on_color: Hsla) -> Stateful<Div> {
    let pal = p();
    div()
        .id(id)
        .flex_none()
        .size(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(R_LG))
        .cursor_pointer()
        .bg(if on { with_alpha(on_color, 0.18) } else { pal.raised })
        .border_1()
        .border_color(if on { with_alpha(on_color, 0.55) } else { border() })
        .hover(|s| s.border_color(border_strong()))
        .active(|s| s.bg(pressed()))
        .child(icon(i, 16., if on { on_color } else { pal.text }))
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
        .rounded(px(R_MD))
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
        .rounded(px(R_SM))
        .cursor_pointer()
        .bg(if on { on_color } else { content(0.06) })
        .when(!on, |d| d.hover(|s| s.bg(content(0.12))))
        .child(
            label(text, TEXT_XS, if on { theme::mix(on_color, gpui::black(), 0.78) } else { pal.muted })
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
        .rounded(px(R_SM))
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
        .rounded(px(R_MD - 1.))
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
        .rounded(px(R_MD + 1.))
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

/// A rotary knob painted as an arc. Drag behaviour is attached by the caller.
pub fn knob(id: impl Into<ElementId>, value: f32, bipolar: bool, side: f32, color: Hsla) -> Stateful<Div> {
    let value = value.clamp(0., 1.);
    div()
        .id(id)
        .flex_none()
        .size(px(side))
        .cursor_ns_resize()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    let c = bounds.center();
                    let r = side / 2. - 3.;
                    let track = content(0.12);
                    paint::arc(window, c, r, KNOB_A0, KNOB_A0 + KNOB_SPAN, 3., track);
                    let (a, b) = if bipolar {
                        let mid = KNOB_A0 + KNOB_SPAN / 2.;
                        let v = KNOB_A0 + KNOB_SPAN * value;
                        (mid.min(v), mid.max(v))
                    } else {
                        (KNOB_A0, KNOB_A0 + KNOB_SPAN * value)
                    };
                    paint::arc(window, c, r, a, b, 3., color);
                    // Body and pointer.
                    let body = r - 4.;
                    window.paint_quad(paint::rrect(
                        gpui::Bounds::new(point(c.x - px(body), c.y - px(body)), gpui::size(px(body * 2.), px(body * 2.))),
                        body,
                        p().raised,
                    ));
                    let ang = KNOB_A0 + KNOB_SPAN * value;
                    let p0 = point(c.x + px((body * 0.25) * ang.cos()), c.y + px((body * 0.25) * ang.sin()));
                    let p1 = point(c.x + px((body - 1.) * ang.cos()), c.y + px((body - 1.) * ang.sin()));
                    paint::polyline(window, &[p0, p1], 2., p().text);
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

/// A vertical fader (position 0..=1) with the 0 dB mark. Drag is the caller's.
pub fn fader(id: impl Into<ElementId>, pos: f32, height: f32, color: Hsla) -> Stateful<Div> {
    let pos = pos.clamp(0., 1.);
    div().id(id).flex_none().w(px(24.)).h(px(height)).cursor_ns_resize().child(
        canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let pal = p();
                let x = b.center().x;
                let top = f32::from(b.origin.y) + 6.;
                let len = f32::from(b.size.height) - 12.;
                let track = gpui::Bounds::new(point(x - px(2.), px(top)), gpui::size(px(4.), px(len)));
                window.paint_quad(paint::rrect(track, 2., content(0.10)));
                let y = top + len * (1. - pos);
                let filled = gpui::Bounds::new(point(x - px(2.), px(y)), gpui::size(px(4.), px(top + len - y)));
                window.paint_quad(paint::rrect(filled, 2., with_alpha(color, 0.7)));
                // Unity mark.
                let unity = top + len * 0.25;
                window.paint_quad(paint::fill_rect(
                    gpui::Bounds::new(point(b.origin.x + px(2.), px(unity)), gpui::size(px(4.), px(1.))),
                    content(0.35),
                ));
                window.paint_quad(paint::fill_rect(
                    gpui::Bounds::new(point(b.origin.x + b.size.width - px(6.), px(unity)), gpui::size(px(4.), px(1.))),
                    content(0.35),
                ));
                let thumb = gpui::Bounds::new(point(x - px(10.), px(y - 5.)), gpui::size(px(20.), px(10.)));
                window.paint_quad(paint::rrect_border(thumb, 3., pal.raised, border_strong(), 1.));
                window.paint_quad(paint::fill_rect(
                    gpui::Bounds::new(point(x - px(6.), px(y - 0.5)), gpui::size(px(12.), px(1.))),
                    pal.text,
                ));
            },
        )
        .size_full(),
    )
}

/// A stereo peak meter, `w` px wide. Pure paint, no elements per segment.
pub fn meter(level: MeterLevel, w: f32, h: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let gap = 2.;
            let bar_w = (f32::from(b.size.width) - gap) / 2.;
            for (i, v) in [level.left, level.right].into_iter().enumerate() {
                let x = f32::from(b.origin.x) + i as f32 * (bar_w + gap);
                let full = f32::from(b.size.height);
                let trough = gpui::Bounds::new(point(px(x), b.origin.y), gpui::size(px(bar_w), b.size.height));
                window.paint_quad(paint::rrect(trough, 1.5, content(0.07)));
                let v = v.clamp(0., 1.);
                if v <= 0.001 {
                    continue;
                }
                // Three zones, each painted only as far as the level reaches.
                for (lo, hi, color) in [(0., 0.72, p().meter_low), (0.72, 0.92, p().meter_mid), (0.92, 1., p().meter_high)] {
                    if v <= lo {
                        break;
                    }
                    let top = v.min(hi);
                    let y0 = f32::from(b.origin.y) + full * (1. - top);
                    let y1 = f32::from(b.origin.y) + full * (1. - lo);
                    window.paint_quad(paint::rrect(
                        gpui::Bounds::new(point(px(x), px(y0)), gpui::size(px(bar_w), px(y1 - y0))),
                        1.,
                        color,
                    ));
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

// ───────────────────────────── Menus ─────────────────────────────

/// The card a popover or context menu draws in, rising in on open.
pub fn menu_card(id: impl Into<ElementId>, width: f32) -> Div {
    let pal = p();
    let _ = id;
    div()
        .w(px(width))
        .p(px(SP1))
        .rounded(px(R_LG))
        .bg(pal.raised)
        .border_1()
        .border_color(border_strong())
        .shadow_lg()
        .flex()
        .flex_col()
        .font_family(FONT_UI)
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
        .rounded(px(R_MD))
        .cursor_pointer()
        .hover(|s| s.bg(hover()))
        .child(div().size(px(16.)).flex_none().flex().items_center().justify_center().when_some(i, |d, i| {
            d.child(icon(i, 14., if checked { pal.accent } else { pal.muted }))
        }))
        .child(label(text, TEXT_MD, if checked { pal.accent } else { pal.text }).flex_1())
        .when_some(hint, |d, h| d.child(mono(h, TEXT_XS, pal.faint)))
        .when(checked, |d| d.child(icon(Icon::Check, 14., pal.accent)))
}

pub fn menu_header(text: &'static str) -> Div {
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
            .rounded(px(R_MD))
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

/// Align helper used by canvases that draw labels.
pub fn align_center() -> TextAlign {
    TextAlign::Center
}
