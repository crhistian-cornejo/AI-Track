//! The DNA panel: every gene of the selected sound, in groups. Drag a bar
//! sideways to change a gene (Shift = fine; the sound previews while you
//! drag and the edit is one undo step on release), double-click it for the
//! gene's default, click a name chip to switch a waveform or filter type.
//! Edits keep the cell where it is: its core is re-based so the edited sound
//! lies on its growth path at the same distance.

use super::view::{self, GeneDrag};
use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_stock::instrument::bloom::genome::{GENES, GROUPS, SOUNDS, Scene, categorical, reflect};
use aitrack_stock::instrument::bloom::grow::replace;
use gpui::{
    AnyElement, Context, DispatchPhase, FontWeight, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    StatefulInteractiveElement, Styled, canvas, div, prelude::FluentBuilder, px,
};

/// Normalised change per pixel of a gene drag.
const PER_PX: f32 = 1. / 180.;

fn set_gene(ws: &mut Workspace, dev: &Dev, scene: &Scene, cell: usize, gene: usize, value: f32, commit: bool) {
    let mut g = scene.sounds[cell];
    g.v[gene] = aitrack_stock::instrument::bloom::genome::quantize(value.clamp(0., 1.));
    let new = replace(scene, cell, &g);
    if commit {
        view::commit(ws, dev, &new, None, "Edit gene");
    } else {
        ws.fx.bloom.view(dev.id).preview = Some(new);
    }
}

fn gene_row(dev: &Dev, scene: &Scene, cell: usize, gene: usize, accent_c: gpui::Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let info = &GENES[gene];
    let g = scene.sounds[cell].folded();
    let value = reflect(g.v[gene]);
    let name = label(info.name, TEXT_XS, pal.muted).w(px(84.)).flex_none();
    if categorical(gene) {
        let now = g.index(gene);
        let mut chips = div().flex_1().flex().flex_wrap().gap(px(SP1));
        for (k, choice) in info.choices.iter().enumerate() {
            let on = k == now;
            let d = dev.clone();
            let target = info.to_normalized(info.min + k as f32);
            chips = chips.child(
                div()
                    .id(("bloom-gene-chip", dev.key((gene << 3) | k)))
                    .h(px(20.))
                    .px(px(SP2))
                    .flex()
                    .items_center()
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .bg(if on { with_alpha(accent_c, 0.22) } else { content(0.05) })
                    .hover(|s| s.bg(content(0.1)))
                    .child(label(*choice, TEXT_XS, if on { pal.text } else { pal.muted }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let s = view::scene(this, &d);
                        set_gene(this, &d, &s, cell, gene, target, true);
                        cx.notify();
                    })),
            );
        }
        return div().flex().items_center().gap(px(SP2)).child(name).child(chips).into_any_element();
    }
    let d = dev.clone();
    let bar = div()
        .id(("bloom-gene", dev.key(gene)))
        .flex_1()
        .h(px(12.))
        .rounded(px(6.))
        .bg(content(0.07))
        .cursor_ew_resize()
        .overflow_hidden()
        .child(div().h_full().w(gpui::relative(value.max(0.02))).rounded(px(6.)).bg(with_alpha(accent_c, 0.75)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let s = view::scene(this, &d);
                if e.click_count >= 2 {
                    set_gene(this, &d, &s, cell, gene, GENES[gene].default_normalized(), true);
                } else {
                    let v0 = reflect(s.sounds[cell].v[gene]);
                    this.fx.bloom.view(d.id).gene_drag = Some(GeneDrag { cell, gene, x0: f32::from(e.position.x), v0 });
                }
                cx.notify();
            }),
        );
    div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(name)
        .child(bar)
        .child(mono(info.format(value), TEXT_XS, pal.text).w(px(64.)).flex_none())
        .into_any_element()
}

/// Window-wide listeners while a gene bar is dragged.
fn capture(dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let weak = cx.entity().downgrade();
    let d = dev.clone();
    canvas(
        |_, _, _| {},
        move |_, _, window, _| {
            let (w, dd) = (weak.clone(), d.clone());
            window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = w.update(cx, |this, cx| {
                    let Some(g) = this.fx.bloom.view(dd.id).gene_drag else { return };
                    let fine = if e.modifiers.shift { 0.2 } else { 1. };
                    let value = g.v0 + (f32::from(e.position.x) - g.x0) * PER_PX * fine;
                    let s = view::scene(this, &dd);
                    set_gene(this, &dd, &s, g.cell, g.gene, value, false);
                    cx.notify();
                });
            });
            let (w, dd) = (weak.clone(), d.clone());
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let _ = w.update(cx, |this, cx| {
                    let v = this.fx.bloom.view(dd.id);
                    if v.gene_drag.take().is_some()
                        && let Some(scene) = v.preview.take()
                    {
                        view::commit(this, &dd, &scene, None, "Edit gene");
                    }
                    cx.notify();
                });
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// The DNA of `cell` in `scene` (the shown scene, so a drag previews here too).
pub fn panel(ws: &mut Workspace, dev: &Dev, scene: &Scene, cell: usize, color: gpui::Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let cell = cell.min(SOUNDS - 1);
    let g = &scene.sounds[cell];
    let d = dev.clone();
    let locked = scene.locked(cell);
    let header = div()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(div().size(px(10.)).rounded_full().bg(color))
        .child(label(format!("{:02} · {}", cell + 1, g.role.label()), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
        .child(div().flex_1())
        .child(mono(format!("d {:.2}", scene.distances[cell]), TEXT_XS, pal.muted))
        .child(
            crate::widgets::icon_chip(("bloom-dna-lock", dev.key(cell)), crate::icons::Icon::Lock, locked, pal.accent)
                .tooltip(crate::widgets::tip("Lock: rolls and mutations leave this sound alone", None))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    view::toggle_lock(this, &d, cell);
                    cx.notify();
                })),
        );
    let mut body = div().id(("bloom-dna", dev.key(0xD7A))).flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_col().gap(px(SP3));
    for (k, (title, genes)) in GROUPS.iter().enumerate() {
        let accent_c = series(k);
        let mut group = div().flex().flex_col().gap(px(SP1)).child(
            div().flex().items_center().gap(px(SP2)).child(div().w(px(3.)).h(px(10.)).rounded(px(2.)).bg(accent_c)).child(label(title.to_uppercase(), TEXT_XS, pal.faint).font_weight(FontWeight::SEMIBOLD)),
        );
        for &gene in *genes {
            group = group.child(gene_row(dev, scene, cell, gene, accent_c, cx));
        }
        body = body.child(group);
    }
    let dragging = ws.fx.bloom.view(dev.id).gene_drag.is_some();
    div()
        .relative()
        .size_full()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_md()))
        .bg(content(0.03))
        .border_1()
        .border_color(border())
        .child(header)
        .child(body)
        .when(dragging, |d| d.child(capture(dev, cx)))
        .into_any_element()
}
