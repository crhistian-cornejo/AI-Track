//! Building blocks of the effect windows (reverb, delay): labelled knob
//! cells, rows of choice chips that show every option at once, and titled
//! sections. Every edit is a `SetParam` command through the device_ui
//! helpers, so the window and the Device View stay in step.

use crate::paint;
use crate::theme::*;
use crate::views::device_ui::{Dev, caption, kit, set_now};
use crate::views::devices::knob_control;
use crate::widgets::{knob_cell, label};
use crate::workspace::{KnobTarget, Workspace};
use aitrack_dsp::ParamScale;
use gpui::{Context, Div, FontWeight, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled, div, px};

/// Width of one knob cell.
pub const CELL_W: f32 = 64.;

/// A labelled knob (or a choice chip for stepped parameters) of `id`.
pub fn control(ws: &mut Workspace, dev: &Dev, id: u32, cx: &mut Context<Workspace>) -> Div {
    let Some(info) = dev.info(id).cloned() else { return div() };
    if matches!(info.scale, ParamScale::Stepped) && !info.choices.is_empty() {
        let _ = ws;
        return div()
            .flex_none()
            .w(px(CELL_W + 16.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SP1))
            .child(label(paint::fit(info.short, CELL_W + 16., TEXT_XS), TEXT_XS, p().muted))
            .child(kit::pick(dev, id, cx));
    }
    let value = dev.norm(id);
    let target = KnobTarget::Param { device: dev.id, param: id, default: info.default_normalized() };
    let k = knob_control(("fxwin-knob", dev.key(id as usize)), value, info.min < 0. && info.max > 0., dev.enabled, target, cx);
    knob_cell(k, info.short, info.format(value), CELL_W)
}

/// A titled group of controls.
pub fn section(ws: &mut Workspace, dev: &Dev, title: &str, ids: &[u32], cx: &mut Context<Workspace>) -> Div {
    let mut row = div().flex().items_start().gap(px(SP1));
    for &id in ids {
        row = row.child(control(ws, dev, id, cx));
    }
    div()
        .flex_none()
        .p(px(SP2))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_md()))
        .bg(p().well)
        .child(caption(title.to_uppercase()))
        .child(row)
}

/// Every choice of a stepped parameter as a chip; the current one lit.
/// `wide` chips suit names ("Concert Hall"), narrow ones short labels.
pub fn choice_row(dev: &Dev, id: u32, wide: bool, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let Some(info) = dev.info(id).cloned() else { return div() };
    let now = dev.choice(id);
    let device = dev.id;
    let mut row = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1));
    for (i, name) in info.choices.iter().enumerate() {
        let on = i == now;
        let norm = info.to_normalized(info.min + i as f32);
        row = row.child(
            div()
                .id(("fxwin-choice", dev.key((id as usize) << 5 | i)))
                .flex_none()
                .h(px(24.))
                .px(px(if wide { SP3 } else { SP2 }))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.22) } else { content(0.05) })
                .border_1()
                .border_color(if on { accent_alpha(0.6) } else { border() })
                .hover(|s| s.bg(if on { accent_alpha(0.3) } else { content(0.1) }))
                .child(label(*name, TEXT_SM, if on { pal.accent } else if dev.enabled { pal.text } else { pal.muted }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    set_now(this, device, id, norm);
                    cx.notify();
                })),
        );
    }
    row
}

/// A caption beside a row of chips.
pub fn labelled(title: &str, row: Div) -> Div {
    div().flex_none().flex().items_center().gap(px(SP2)).child(caption(title.to_uppercase())).child(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_on_the_spacing_grid() {
        assert_eq!(CELL_W % 4., 0.);
        assert_eq!((CELL_W + 16.) % 4., 0.);
    }
}
