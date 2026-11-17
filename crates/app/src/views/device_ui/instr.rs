//! Building blocks of the instrument windows (kick, acid, drum, sampler):
//! the editor's root, a title row with read-outs, a drawing surface that
//! fills the room it gets, titled groups of knobs, and the knob and choice
//! chips themselves. Every control sends the same `SetParam` commands as the
//! Device View cards, so the card and the window always agree.

use super::{Dev, caption, kit};
use crate::paint;
use crate::theme::*;
use crate::views::devices::knob_control_sized;
use crate::widgets::{self, knob_cell, label, mono};
use crate::workspace::{KnobTarget, Workspace};
use gpui::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div,
    prelude::FluentBuilder, px,
};

/// Diameter of a knob in a group.
pub const DIAL: f32 = 40.;
/// Width of the cell a knob sits in (a name above, its value below).
pub const CELL: f32 = DIAL + 24.;

/// The root of an instrument's window editor: fills the window (see
/// [`kit::panel`]) with roomier padding than a card has.
pub fn root(min_w: f32) -> Div {
    kit::panel(min_w).p(px(SP4)).gap(px(SP3))
}

/// A drawing surface: it takes all the room its parent leaves and the plot
/// inside grows with it. `min_h` is the least it keeps before the window scrolls.
pub fn stage(min_h: f32) -> Div {
    div().flex_1().min_w(px(0.)).min_h(px(min_h)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden()
}

/// A title with read-outs on its right.
pub fn title(text: impl Into<gpui::SharedString>, readouts: Vec<Div>) -> Div {
    div().flex_none().flex().items_center().gap(px(SP2)).child(caption(text)).child(div().flex_1()).children(readouts)
}

/// A titled group of controls, with a stripe in `accent` before the title.
pub fn group(title: &str, accent: Hsla, body: Vec<AnyElement>) -> Div {
    let pal = p();
    // Each group takes its line's free width (see `kit::flow`), so a wider window
    // spreads them and a narrower one wraps them: no dead space, nothing cut.
    kit::grow(div().flex_shrink_0())
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_md()))
        .bg(content(0.03))
        .border_1()
        .border_color(border())
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(div().w(px(3.)).h(px(12.)).rounded(px(2.)).bg(accent))
                .child(label(title.to_owned(), TEXT_XS, pal.muted).font_weight(FontWeight::SEMIBOLD)),
        )
        .child(div().flex().items_start().justify_around().gap(px(SP2)).children(body))
}

/// A wrapping row of groups.
pub fn groups(items: Vec<Div>) -> Div {
    kit::flow().children(items)
}

/// A labelled knob of parameter `id`: its short name above, the dial, its
/// value below. Drag turns it (Shift = fine), double-click resets, right-click
/// opens the knob's menu. A stepped parameter shows as chips instead.
pub fn dial(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    if matches!(info.scale, aitrack_dsp::ParamScale::Stepped) && !info.choices.is_empty() {
        return chips(dev, id, cx);
    }
    let value = dev.norm(id);
    let target = KnobTarget::Param { device: dev.id, param: id, default: info.default_normalized() };
    let k = knob_control_sized(("inst-knob", dev.key(id as usize)), value, info.min < 0. && info.max > 0., dev.enabled, DIAL, target, cx)
        .tooltip(widgets::tip(info.name, Some("Drag · Shift fine · double-click resets")));
    knob_cell(k, info.short, info.format(value), CELL).into_any_element()
}

/// Every choice of a stepped parameter as a chip under its name; the
/// current one is lit. A switch (`Off` / `On`) is a single chip.
pub fn chips(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    let pal = p();
    let (now, device) = (dev.choice(id), dev.id);
    let mut row = div().flex().flex_wrap().items_center().gap(px(SP1));
    for (i, name) in info.choices.iter().enumerate() {
        let on = i == now;
        let norm = info.to_normalized(info.min + i as f32);
        row = row.child(
            div()
                .id(("inst-chip", dev.key((id as usize) << 4 | i)))
                .flex_none()
                .h(px(24.))
                .px(px(SP2))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.22) } else { content(0.05) })
                .border_1()
                .border_color(if on { accent_alpha(0.6) } else { border() })
                .hover(|s| s.bg(if on { accent_alpha(0.3) } else { content(0.1) }))
                .child(label(*name, TEXT_SM, if on { pal.accent } else if dev.enabled { pal.text } else { pal.muted }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    super::set_now(this, device, id, norm);
                    cx.notify();
                })),
        );
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(label(paint::fit(info.name, 140., TEXT_XS), TEXT_XS, pal.muted))
        .child(row)
        .into_any_element()
}

/// A knob per id, in a group row.
pub fn dials(dev: &Dev, ids: &[u32], cx: &mut Context<Workspace>) -> Vec<AnyElement> {
    ids.iter().map(|&id| dial(dev, id, cx)).collect()
}

/// A read-out in a title row: highlighted while `live`.
pub fn tag(name: &str, value: String, color: Hsla, live: bool) -> Div {
    kit::readout(name, value, color).when(live, |d| d.border_color(with_alpha(color, 0.6)))
}

/// A small caption-and-text pair for hints under a stage.
pub fn hint(text: &str) -> Div {
    div().flex_none().child(mono(text.to_owned(), TEXT_XS, p().faint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knob_cells_sit_on_the_spacing_grid() {
        assert_eq!(DIAL % 4., 0.);
        assert_eq!(CELL % 4., 0.);
    }
}
