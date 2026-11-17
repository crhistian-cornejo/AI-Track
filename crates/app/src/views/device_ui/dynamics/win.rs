//! What the dynamics and saturation editors share in a device window: knob
//! cells with a readable value, titled groups of them, and the plot / controls
//! arrangement. The window already shows the input, gain reduction and output
//! levels in its frame, so these editors keep inside only what the frame does
//! not give (curves, histories, per-band levels).
//!
//! Each editor keeps its compact card for the Device View and builds its
//! window arrangement from these parts. Every edit still goes through the
//! device_ui helpers (`SetParam` commands), so the window and the card never
//! disagree.

use crate::paint;
use crate::theme::*;
use crate::views::device_ui::{Dev, Layout, kit};
use crate::views::devices::knob_control_sized;
use crate::widgets::{label, mono};
use crate::workspace::{KnobTarget, Workspace};
use aitrack_dsp::ParamScale;
use gpui::{AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px};

/// Width of one knob cell and the knob's diameter.
pub const CELL_W: f32 = 68.;
pub const KNOB: f32 = 44.;
/// The value under a knob.
const VALUE_SIZE: f32 = 13.;

/// Whether the editor being built is for a device window.
pub fn in_window(dev: &Dev) -> bool {
    dev.layout == Layout::Window
}

/// The control of one parameter as a cell of a group: a knob with its name
/// above and its value below; a chip (with its name above when it has more
/// than two choices) for a stepped parameter; a bare chip for a switch.
pub fn knob(dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let Some(info) = dev.info(id).cloned() else { return div().into_any_element() };
    if matches!(info.scale, ParamScale::Stepped) && !info.choices.is_empty() {
        if info.choices == ["Off", "On"] {
            return div().flex_none().h(px(KNOB + 34.)).flex().items_center().child(kit::pick(dev, id, cx)).into_any_element();
        }
        return div()
            .flex_none()
            .h(px(KNOB + 34.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SP1))
            .child(label(info.short, TEXT_SM, pal.muted))
            .child(kit::pick(dev, id, cx))
            .into_any_element();
    }
    let value = dev.norm(id);
    let target = KnobTarget::Param { device: dev.id, param: id, default: info.default_normalized() };
    let k = knob_control_sized(("dyn-knob", dev.key(id as usize)), value, info.min < 0. && info.max > 0., dev.enabled, KNOB, target, cx);
    div()
        .flex_none()
        .w(px(CELL_W))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(label(paint::fit(info.short, CELL_W, TEXT_SM), TEXT_SM, pal.muted))
        .child(k)
        .child(mono(paint::fit(&info.format(value), CELL_W, VALUE_SIZE), VALUE_SIZE, if dev.enabled { pal.text } else { pal.muted }).font_weight(gpui::FontWeight::SEMIBOLD))
        .into_any_element()
}

/// A card: one surface, no border, a small title over its controls.
fn card(dev: &Dev, title: &str, switch: Option<u32>, body: Vec<AnyElement>, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let on = switch.is_none_or(|id| dev.on(id));
    let device = dev.id;
    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(SP2))
        .child(label(title.to_uppercase(), TEXT_SM, if on { pal.muted } else { pal.faint }).font_weight(gpui::FontWeight::SEMIBOLD))
        .when_some(switch, |d, id| {
            d.child(
                crate::widgets::switch(("win-card-on", dev.key(id as usize)), on)
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        crate::views::device_ui::set_now(this, device, id, if on { 0. } else { 1. });
                        cx.notify();
                    })),
            )
        });
    div()
        .flex_none()
        .max_w(gpui::relative(1.))
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_md()))
        .bg(content(0.04))
        .child(header)
        .child(div().flex().flex_wrap().items_center().justify_around().gap(px(SP2)).when(!on, |d| d.opacity(0.45)).children(body))
}

/// A titled card of controls, with an optional on/off switch for the stage.
pub fn group(dev: &Dev, title: &str, switch: Option<u32>, ids: &[u32], cx: &mut Context<Workspace>) -> Div {
    let body = ids.iter().map(|id| knob(dev, *id, cx)).collect();
    card(dev, title, switch, body, cx)
}

/// A group whose body is already built (a source chip above the knobs, say).
pub fn group_of(dev: &Dev, title: &str, switch: Option<u32>, body: Vec<AnyElement>, cx: &mut Context<Workspace>) -> Div {
    card(dev, title, switch, body, cx)
}

/// A full-width row of chips that sits at the top of a group's body.
pub fn chip_row(chips: Vec<AnyElement>) -> AnyElement {
    div().w_full().flex().flex_wrap().items_center().gap(px(SP1)).children(chips).into_any_element()
}

/// A slot for a plot that reads best square (a transfer curve): as wide as the
/// row is tall, as of the last frame, within what the window can spare.
pub fn square(ws: &mut Workspace, dev: &Dev, slot: u8, child: impl IntoElement) -> Div {
    let h = f32::from(ws.fx.region(dev.id, slot).get().size.height);
    let side = if h > 8. { h.clamp(240., 560.) } else { 320. };
    div().flex_none().h_full().w(px(side)).max_w(gpui::relative(0.55)).child(child)
}

/// A caption beside a control on the editor's top bar.
pub fn captioned(title: &str, control: AnyElement) -> Div {
    div().flex_none().flex().items_center().gap(px(SP2)).child(crate::views::device_ui::caption(title.to_uppercase())).child(control)
}

/// The slot of a device's "More" drawer in its view options (remembered while the app runs).
const MORE_SLOT: u8 = 6;

/// Whether the device's "More" drawer is open.
pub fn more_open(ws: &Workspace, dev: &Dev) -> bool {
    // `AITRACK_DEV_MORE` opens every drawer (screenshots).
    std::env::var_os("AITRACK_DEV_MORE").is_some() || ws.fx.opt(dev.id, MORE_SLOT, 0.) > 0.5
}

/// The window's arrangement: `top` (chips, badges), the displays (they take
/// every pixel the controls leave), one row of primary groups spread across the
/// full width and, when `more` has anything, a "More" button that opens a drawer
/// with the rest. `min_w` is the least width it needs; below it the window scrolls.
#[allow(clippy::too_many_arguments)]
pub fn arrange(ws: &Workspace, dev: &Dev, min_w: f32, top: Option<Div>, displays: Div, primary: Vec<Div>, more: Vec<Div>, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let mut root = kit::panel(min_w);
    if let Some(top) = top {
        root = root.child(top);
    }
    let open = more_open(ws, dev);
    let more_len = more.len();
    let device = dev.id;
    // The groups share the row's width (they grow into the room), so nothing is left as a gap.
    let mut row = div().flex_none().flex().flex_wrap().items_stretch().gap(px(SP2)).children(primary.into_iter().map(|g| g.flex_grow(1.)));
    if !more.is_empty() {
        let toggle = div()
            .id(("win-more", dev.key(0)))
            .flex_none()
            .self_start()
            .h(px(28.))
            .px(px(SP3))
            .flex()
            .items_center()
            .gap(px(SP1))
            .rounded(px(r_sm()))
            .cursor_pointer()
            .bg(if open { accent_alpha(0.2) } else { content(0.06) })
            .hover(|s| s.bg(content(0.12)))
            .child(label(if open { "Less" } else { "More" }, TEXT_MD, if open { pal.accent } else { pal.text }).font_weight(gpui::FontWeight::MEDIUM))
            .child(crate::icons::icon(if open { crate::icons::Icon::ChevronUp } else { crate::icons::Icon::ChevronDown }, 10., pal.faint))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, _, cx| {
                let was = this.fx.opt(device, MORE_SLOT, 0.) > 0.5;
                this.fx.opts.insert((device, MORE_SLOT), if was { 0. } else { 1. });
                cx.notify();
            }));
        row = row.child(toggle);
    }
    let mut root = root.child(displays.flex_1().min_h(px(160.)).flex().gap(px(SP2))).child(row);
    if open && !more.is_empty() {
        // The drawer is the frame's: the window grows by its height while it is open.
        root = root.child(kit::drawer(ws, dev, more.into_iter().map(kit::grow).collect(), 130. * (more_len.div_ceil(2)) as f32));
    }
    root
}

/// The trace legend of a history: a coloured dot per name, in a row.
pub fn legend(items: &[(&str, gpui::Hsla)]) -> Div {
    let mut row = div().flex_none().flex().items_center().gap(px(SP3));
    for (name, color) in items {
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(div().size(px(8.)).rounded_full().bg(*color))
                .child(label((*name).to_owned(), TEXT_SM, p().muted)),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_on_the_spacing_grid() {
        assert_eq!(CELL_W % 4., 0.);
        assert_eq!(KNOB % 4., 0.);
    }
}
