//! `instrument.drum`: a 4×4 pad grid (bottom-left first, like hardware)
//! with each pad's waveform and level, and the selected pad's sample with
//! its decay drawn on it (drag the handle: decay sideways, level up/down).

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, caption};
use crate::theme::*;
use crate::widgets::mono;
use crate::workspace::Workspace;
use aitrack_model::Device;
use aitrack_stock::instrument::drum::{PADS, p};
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, TextAlign, canvas, div, px};

const PAD: f32 = 44.;

/// Grid cell of pad `i`: `(column, row from the top)`.
pub fn pad_cell(i: usize) -> (usize, usize) {
    (i % 4, 3 - i / 4)
}

pub fn render(ws: &mut Workspace, dev: &Dev, d: &Device, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (_, paths) = kit::state_paths(d.state.as_deref());
    let selected = dev.selected.min(PADS - 1);
    let device = dev.id;
    let waves: Vec<_> = (0..PADS).map(|i| paths.get(i).and_then(|p| p.as_deref()).map(|p| ws.fx.waves.get(p)).unwrap_or_default()).collect();
    let last_hit = if dev.meter.len() > 1 { dev.m(1) as i32 } else { -1 };

    let mut grid = div().flex_none().relative().w(px(4. * PAD + 3. * SP1)).h(px(4. * PAD + 3. * SP1));
    for (i, wave) in waves.iter().enumerate() {
        let (col, row) = pad_cell(i);
        let (wave, level) = (wave.clone(), dev.m(2 + i).clamp(0., 1.));
        let on = i == selected;
        let hit = last_hit == i as i32 && level > 0.02;
        grid = grid.child(
            div()
                .id(("drum-pad", dev.key(i)))
                .absolute()
                .left(px(col as f32 * (PAD + SP1)))
                .top(px(row as f32 * (PAD + SP1)))
                .size(px(PAD))
                .rounded(px(r_md()))
                .overflow_hidden()
                .cursor_pointer()
                .bg(if wave.is_empty() { content(0.04) } else { content(0.08) })
                .border_1()
                .border_color(if on { pal.accent } else if hit { with_alpha(pal.accent2, 0.8) } else { border() })
                .hover(|s| s.bg(content(0.14)))
                .child(
                    canvas(
                        |_, _, _| {},
                        move |b, _, window, cx| {
                            let pal = p();
                            let r = Rect::of(b);
                            // How hard the pad was just hit.
                            kit::bar(window, r.x, r.bottom() - r.h * level, r.w, r.h * level, 0., accent_alpha(0.28));
                            kit::paint_wave(window, r.inset(3.), &wave, if on { pal.text } else { pal.muted });
                            kit::txt(window, cx, &format!("{}", i + 1), r.x + 4., r.y + 2., TEXT_XS, if on { pal.accent } else { pal.faint }, TextAlign::Left);
                        },
                    )
                    .size_full(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fx.selected.insert(device, i);
                    cx.notify();
                })),
        );
    }

    // The selected pad.
    let id = |x: u32| p::pad(selected as u32, x);
    let default = |x: u32| (x, dev.info(x).map_or(0.5, |i| i.default_normalized()));
    let (decay, level) = (dev.norm(id(p::DECAY)), dev.norm(id(p::LEVEL)));
    let handles = vec![Handle {
        fx: 0.08 + 0.88 * decay,
        fy: 0.88 - 0.76 * level,
        x: ax(id(p::DECAY), 1. / 0.88),
        y: ax(id(p::LEVEL), -1. / 0.76),
        index: selected,
        reset: vec![default(id(p::DECAY)), default(id(p::LEVEL))],
        ..Default::default()
    }];
    let (wave, enabled) = (waves[selected].clone(), dev.enabled);
    let name = paths.get(selected).and_then(|p| p.as_deref()).and_then(|p| std::path::Path::new(p).file_stem()).map(|n| n.to_string_lossy().into_owned());
    let pad_plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        if wave.is_empty() {
            kit::txt(window, cx, "Empty pad", r.px(0.5), r.py(0.5) - 7., TEXT_SM, pal.faint, TextAlign::Center);
        } else {
            kit::paint_wave(window, r.inset(2.), &wave, if enabled { pal.waveform } else { pal.muted });
        }
        // The decay envelope over the sample, scaled by the pad level.
        let (end, top) = (0.08 + 0.88 * decay, 0.88 - 0.76 * level);
        let env = [r.pt(0., top), r.pt(end * 0.35, top + (0.88 - top) * 0.3), r.pt(end, 0.88)];
        kit::line(window, &env, 2., pal.accent);
        kit::dot(window, r.pt(end, top), 6.5, pal.accent, Some(pal.text));
        if let Some(name) = &name {
            kit::txt(window, cx, name, r.right() - 6., r.y + 3., TEXT_XS, pal.muted, TextAlign::Right);
        }
    });
    let pad = div()
        .flex_1()
        .min_w(px(0.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(div().flex().items_center().gap(px(SP2)).child(caption(format!("PAD {}", selected + 1))).child(div().flex_1()).child(kit::pick(dev, id(p::CHOKE), cx)).child(kit::pick(dev, id(p::GATE), cx)))
        .child(div().flex_1().min_h(px(0.)).child(pad_plot))
        .child(kit::strip(dev, &[id(p::TUNE), id(p::FINE), id(p::DECAY), id(p::CUTOFF), id(p::PAN), id(p::LEVEL)], &[], cx));
    let voices = dev.m(0);
    let global = div()
        .flex_none()
        .w(px(72.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(caption("KIT"))
        .child(kit::num(dev, p::VOLUME, cx))
        .child(kit::num(dev, p::VEL_SENS, cx))
        .child(kit::pick(dev, p::VEL_CURVE, cx))
        .child(div().flex_1())
        .child(mono(format!("{voices:.0} voices"), TEXT_XS, pal.faint));
    div().flex_none().w(px(if dev.big { 760. } else { 600. })).h_full().p(px(SP2)).gap(px(SP3)).flex().child(grid).child(pad).child(global).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_fill_the_grid_from_the_bottom_left() {
        assert_eq!(pad_cell(0), (0, 3));
        assert_eq!(pad_cell(3), (3, 3));
        assert_eq!(pad_cell(4), (0, 2));
        assert_eq!(pad_cell(15), (3, 0));
        let mut cells: Vec<_> = (0..PADS).map(pad_cell).collect();
        cells.sort();
        cells.dedup();
        assert_eq!(cells.len(), 16);
    }
}
