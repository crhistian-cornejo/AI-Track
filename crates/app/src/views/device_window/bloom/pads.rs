//! The sixteen sounds as pads under the stage: each in its own colour with
//! its number and role, lit while it plays, ringed when selected, a lock
//! when locked. Click a pad to select and hear it, Alt-click to lock it.

use super::look::Look;
use super::view;
use super::visual::audition;
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_stock::instrument::bloom::genome::{SOUNDS, Scene};
use aitrack_stock::instrument::bloom::p;
use gpui::{AnyElement, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled, div, point, px};

/// The height of a pad.
pub const PAD_H: f32 = 44.0;
/// A pad is never narrower than this.
pub const PAD_MIN_W: f32 = 40.0;

/// How many rows sixteen pads need in `width` px (one when they fit, else two).
pub fn rows(width: f32) -> usize {
    if width >= SOUNDS as f32 * (PAD_MIN_W + SP1) { 1 } else { 2 }
}

/// The pads, `cols` per row (16 or 8).
pub fn pads(ws: &mut Workspace, dev: &Dev, scene: &Scene, colours: &[Look; SOUNDS], cols: usize, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let selected = dev.choice(p::SELECTED).min(SOUNDS - 1);
    let matching = ws.fx.bloom.view(dev.id).matching.as_ref().map(|m| (m.cell, m.fraction));
    let names = view::names(ws, dev, scene);
    let locks: [bool; SOUNDS] = std::array::from_fn(|i| view::locked(ws, dev, scene, i));
    let mut grid = div().w_full().flex().flex_col().gap(px(SP1));
    let cols = cols.clamp(1, SOUNDS);
    for row in 0..SOUNDS.div_ceil(cols) {
        let mut line = div().w_full().flex().gap(px(SP1));
        for (i, look) in colours.iter().enumerate().take(((row + 1) * cols).min(SOUNDS)).skip(row * cols) {
            let c = super::stage::colour(look);
            let surface = pal.surface;
            let on = i == selected;
            let lit = dev.m(i).clamp(0.0, 1.0);
            let locked = locks[i];
            let name = names[i].clone();
            let d = dev.clone();
            let caption = match matching {
                Some((cell, f)) if cell == i => mono(format!("{:.0}%", f * 100.), TEXT_XS, pal.accent2),
                _ => label(name.clone(), TEXT_XS, if on { pal.text } else { pal.muted }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }),
            };
            let mut pad = div()
                .id(("bloom-pad", dev.key(0x500 | i)))
                .relative()
                .flex_1()
                .min_w(px(PAD_MIN_W))
                .h(px(PAD_H))
                .px(px(SP1))
                .pt(px(SP1))
                .flex()
                .flex_col()
                .justify_between()
                .overflow_hidden()
                .rounded(px(r_md()))
                .cursor_pointer()
                .bg(mix(pal.surface, c, if on { 0.22 } else { 0.08 + 0.3 * lit }))
                .border_1()
                .border_color(if on { c } else { with_alpha(c, 0.25 + 0.5 * lit) })
                .hover(move |s| s.bg(mix(surface, c, 0.3)))
                // The sound's colour as a bar along the top, brighter while it plays.
                .child(div().absolute().top_0().left_0().right_0().h(px(3.)).bg(with_alpha(c, 0.55 + 0.45 * lit.max(if on { 1.0 } else { 0.0 }))))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SP1))
                        .child(mono(format!("{:02}", i + 1), TEXT_XS, if on { pal.text } else { pal.faint }))
                        .child(div().flex_1())
                        .when_locked(locked, pal.accent),
                )
                .child(div().pb(px(SP1)).overflow_hidden().child(caption))
                .tooltip(widgets::tip_owned(format!("{:02} {} — click: hear · Alt-click: lock", i + 1, name)))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if e.modifiers.alt {
                        view::toggle_lock(this, &d, i);
                        cx.notify();
                        return;
                    }
                    this.fx.bloom.view(d.id).dna_cell = Some(i);
                    audition(this, &d, i, cx);
                }))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.release_audition(cx)));
            if lit > 0.02 {
                pad = pad.shadow(vec![gpui::BoxShadow { color: with_alpha(c, 0.6 * lit), offset: point(px(0.), px(0.)), blur_radius: px(4. + 10. * lit), spread_radius: px(0.), inset: false }]);
            }
            line = line.child(pad);
        }
        grid = grid.child(line);
    }
    grid.into_any_element()
}

trait WhenLocked {
    fn when_locked(self, locked: bool, color: gpui::Hsla) -> Self;
}

impl WhenLocked for gpui::Div {
    fn when_locked(self, locked: bool, color: gpui::Hsla) -> Self {
        if locked { self.child(icon(Icon::Lock, 10., color)) } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_take_one_row_when_wide_and_two_when_narrow() {
        assert_eq!(rows(1200.), 1);
        assert_eq!(rows(500.), 2);
        assert_eq!(PAD_H % 4., 0.);
        assert_eq!(PAD_MIN_W % 4., 0.);
    }
}
