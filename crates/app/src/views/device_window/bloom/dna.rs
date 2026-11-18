//! The gene editor: every gene of the selected sound as a small knob in the
//! sound's colour, in groups (Pitch, Oscillators, FM, Noise, Filter, Amp,
//! Resonator, Output) that flow across the panel and fold away. Drag a knob
//! (right or up raises it, Shift is fine; the sound previews while you drag
//! and the edit is one undo step on release), double-click it for the gene's
//! default; a switch (a waveform, a filter type) steps on click, back with
//! Alt. Edits keep the cell where it is: its core is re-based so the edited
//! sound lies on its growth path at the same distance.

use super::view::{self, GeneDrag};
use crate::icons::{Icon, icon};
use crate::theme::*;
use crate::views::device_ui::Dev;
use crate::widgets::{self, knob, label, mono};
use crate::workspace::Workspace;
use aitrack_stock::instrument::bloom::genome::{GENES, GROUPS, SOUNDS, Scene, categorical, reflect};
use aitrack_stock::instrument::bloom::grow::replace;
use gpui::{
    AnyElement, Context, DispatchPhase, Div, FontWeight, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    StatefulInteractiveElement, Styled, canvas, div, prelude::FluentBuilder, px,
};

/// Normalised change per pixel of a gene drag.
const PER_PX: f32 = 1. / 180.;
/// A gene knob's size and its cell's width.
pub const GENE_KNOB: f32 = 24.;
pub const GENE_W: f32 = 56.;

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

/// The gene drag's value after the pointer moved by (`dx`, `dy`): right or up raises it.
pub fn dragged(v0: f32, dx: f32, dy: f32, fine: bool) -> f32 {
    (v0 + (dx - dy) * PER_PX * if fine { 0.2 } else { 1. }).clamp(0., 1.)
}

/// The next choice of a switch with `n` choices (`back`: the previous one).
pub fn step_choice(now: usize, n: usize, back: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if back { (now + n - 1) % n } else { (now + 1) % n }
}

/// One gene: its name, a knob (or a switch), its value.
fn gene_cell(dev: &Dev, scene: &Scene, cell: usize, gene: usize, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let info = &GENES[gene];
    let g = scene.sounds[cell].folded();
    let value = reflect(g.v[gene]);
    let name = label(crate::paint::fit(info.name, GENE_W, TEXT_XS), TEXT_XS, pal.muted);
    let d = dev.clone();
    if categorical(gene) {
        let now = g.index(gene);
        let n = info.choices.len();
        let switch = div()
            .id(("bloom-gene-switch", dev.key(gene)))
            .h(px(GENE_KNOB))
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(r_sm()))
            .cursor_pointer()
            .bg(with_alpha(color, 0.14))
            .border_1()
            .border_color(with_alpha(color, 0.4))
            .hover(|s| s.bg(content(0.1)))
            .child(label(crate::paint::fit(info.choices.get(now).copied().unwrap_or(""), GENE_W - 8., TEXT_XS), TEXT_XS, pal.text))
            .tooltip(widgets::tip_owned(format!("{} — click: next · Alt-click: previous", info.name)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                let k = step_choice(now, n, e.modifiers().alt);
                let target = GENES[gene].to_normalized(GENES[gene].min + k as f32);
                let s = view::scene(this, &d);
                set_gene(this, &d, &s, cell, gene, target, true);
                cx.notify();
            }));
        return div().w(px(GENE_W)).flex().flex_col().items_center().gap(px(2.)).child(name).child(switch).child(div().h(px(14.))).into_any_element();
    }
    let k = knob(("bloom-gene", dev.key(gene)), value, false, GENE_KNOB, color)
        .tooltip(widgets::tip_owned(format!("{} — drag · double-click resets", info.name)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                let s = view::scene(this, &d);
                if e.click_count >= 2 {
                    set_gene(this, &d, &s, cell, gene, GENES[gene].default_normalized(), true);
                } else {
                    let v0 = reflect(s.sounds[cell].v[gene]);
                    this.fx.bloom.view(d.id).gene_drag = Some(GeneDrag { cell, gene, x0: f32::from(e.position.x), y0: f32::from(e.position.y), v0 });
                }
                cx.notify();
            }),
        );
    div()
        .w(px(GENE_W))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(name)
        .child(k)
        .child(mono(crate::paint::fit(&info.format(value), GENE_W, TEXT_XS), TEXT_XS, pal.text))
        .into_any_element()
}

/// Window-wide listeners while a gene knob is dragged.
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
                    let value = dragged(g.v0, f32::from(e.position.x) - g.x0, f32::from(e.position.y) - g.y0, e.modifiers.shift);
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

/// One group of genes: a title that folds it, its genes in a row.
fn group(dev: &Dev, scene: &Scene, cell: usize, k: usize, folded: bool, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let (title, genes) = GROUPS[k];
    let d = dev.clone();
    let head = div()
        .id(("bloom-gene-group", dev.key(0x680 | k)))
        .flex()
        .items_center()
        .gap(px(SP1))
        .cursor_pointer()
        .child(icon(if folded { Icon::ChevronRight } else { Icon::ChevronDown }, 10., pal.faint))
        .child(label(title.to_uppercase(), TEXT_XS, if folded { pal.faint } else { pal.muted }).font_weight(FontWeight::SEMIBOLD))
        .when(folded, |h| h.child(mono(format!("{}", genes.len()), TEXT_XS, pal.faint)))
        .tooltip(widgets::tip(if folded { "Show these genes" } else { "Fold these genes" }, None))
        .on_click(cx.listener(move |this, _, _, cx| {
            let st = &mut this.fx.bloom.view(d.id).stage;
            st.folded ^= 1 << k;
            cx.notify();
        }));
    let mut body = div().flex().gap(px(SP1));
    if !folded {
        for &gene in genes {
            body = body.child(gene_cell(dev, scene, cell, gene, color, cx));
        }
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .p(px(SP2))
        .rounded(px(r_md()))
        .bg(content(0.03))
        .border_1()
        .border_color(border())
        .child(head)
        .when(!folded, |g| g.child(body))
}

/// The genes of `cell` in `scene` (the shown scene, so a drag previews here too).
pub fn panel(ws: &mut Workspace, dev: &Dev, scene: &Scene, cell: usize, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let cell = cell.min(SOUNDS - 1);
    let g = &scene.sounds[cell];
    let folded = ws.fx.bloom.view(dev.id).stage.folded;
    let header = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP2))
        .child(div().size(px(10.)).rounded_full().bg(color))
        .child(label(format!("{:02} {}", cell + 1, g.role.label()), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
        .child(label("genes", TEXT_XS, pal.faint))
        .child(div().flex_1())
        .child(mono(format!("{:.2} from its core", scene.distances[cell]), TEXT_XS, pal.muted));
    let mut body = div().id(("bloom-dna", dev.key(0xD7A))).flex_1().min_h(px(0.)).overflow_y_scroll().flex().flex_wrap().content_start().gap(px(SP2));
    for k in 0..GROUPS.len() {
        body = body.child(group(dev, scene, cell, k, folded & (1 << k) != 0, color, cx));
    }
    let dragging = ws.fx.bloom.view(dev.id).gene_drag.is_some();
    div()
        .relative()
        .size_full()
        .p(px(SP3))
        .gap(px(SP2))
        .flex()
        .flex_col()
        .rounded(px(r_lg()))
        .bg(content(0.03))
        .border_1()
        .border_color(border())
        .child(header)
        .child(body)
        .when(dragging, |d| d.child(capture(dev, cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_right_or_up_raises_a_gene_and_shift_is_fine() {
        assert!(dragged(0.5, 18., 0., false) > 0.5);
        assert!(dragged(0.5, 0., -18., false) > 0.5);
        assert!(dragged(0.5, 0., 18., false) < 0.5);
        assert!((dragged(0.5, 18., 0., true) - 0.5).abs() < (dragged(0.5, 18., 0., false) - 0.5).abs());
        assert_eq!(dragged(0.9, 1000., 0., false), 1.);
        assert_eq!(dragged(0.1, -1000., 0., false), 0.);
    }

    #[test]
    fn switches_step_round_both_ways() {
        assert_eq!(step_choice(0, 4, false), 1);
        assert_eq!(step_choice(3, 4, false), 0);
        assert_eq!(step_choice(0, 4, true), 3);
        assert_eq!(step_choice(0, 0, true), 0);
    }

    #[test]
    fn gene_cells_sit_on_the_grid_and_every_gene_is_in_a_group() {
        assert_eq!(GENE_W % 4., 0.);
        assert_eq!(GENE_KNOB % 4., 0.);
        let mut seen: Vec<usize> = GROUPS.iter().flat_map(|(_, g)| g.iter().copied()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), GENES.len());
        assert!(GROUPS.len() <= 16, "a fold bit per group");
    }
}
