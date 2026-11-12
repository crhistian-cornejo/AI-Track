//! `fx.multiband`: the three bands laid out over frequency — the crossovers
//! are draggable lines, each band's gain is a draggable level line, and the
//! per-band mute / solo / delta switches sit under the display.

use super::kit::{self, F_HI, F_LO, Handle, PlotCfg, ax, freq_frac};
use super::{Dev, caption};
use crate::theme::*;
use crate::widgets::label;
use crate::workspace::Workspace;
use aitrack_stock::fx::multiband::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Handle indices of the two crossovers (bands are 0..3).
const XOVER: usize = 8;

/// Band edges in Hz: `[20, low, high, 20k]`, sorted and kept apart like the
/// plugin does.
pub fn edges(low: f32, high: f32) -> [f32; 4] {
    let (a, b) = if low <= high {
        (low, high)
    } else {
        (high, low)
    };
    let lo = a.clamp(F_LO, F_HI);
    let hi = b.max(lo * 1.2).clamp(F_LO, F_HI);
    [F_LO, lo, hi, F_HI]
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let selected = match dev.selected {
        0..=2 => dev.selected,
        8 => 0,
        9 => 1,
        _ => 0,
    };
    let e = edges(dev.plain(p::XOVER_LOW), dev.plain(p::XOVER_HIGH));
    let gain: Vec<f32> = (0..3).map(|b| dev.plain(p::band(b, p::GAIN))).collect();
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let mut handles = Vec::new();
    for b in 0..3 {
        let center = (e[b] * e[b + 1]).sqrt();
        handles.push(Handle {
            fx: freq_frac(center),
            fy: (0.5 - gain[b] / 48.).clamp(0.03, 0.97),
            y: ax(p::band(b as u32, p::GAIN), -1.),
            index: b,
            reset: vec![default(p::band(b as u32, p::GAIN))],
            ..Default::default()
        });
    }
    for (i, id) in [p::XOVER_LOW, p::XOVER_HIGH].into_iter().enumerate() {
        let span = dev.info(id).map_or(1., kit::log_span);
        handles.push(Handle {
            fx: freq_frac(dev.plain(id)),
            fy: 0.5,
            x: ax(id, span),
            index: XOVER + i,
            reset: vec![default(id)],
            ..Default::default()
        });
    }
    let lines: Vec<f32> = (0..2)
        .map(|i| freq_frac(dev.plain([p::XOVER_LOW, p::XOVER_HIGH][i])))
        .collect();
    let gains = gain;
    let levels: Vec<f32> = (0..3).map(|b| dev.m(b)).collect();
    let enabled = dev.enabled;
    let names = ["LOW", "MID", "HIGH"];
    let plot = kit::plot(
        ws,
        dev,
        PlotCfg {
            slot: 0,
            handles,
            keys: true,
            ..Default::default()
        },
        cx,
        move |r, window, cx| {
            let pal = p();
            kit::freq_grid(window, cx, r, true);
            for b in 0..3 {
                let a = r.px(freq_frac(e[b]));
                let z = r.px(freq_frac(e[b + 1]));
                let color = series(b);
                kit::bar(
                    window,
                    a,
                    r.y,
                    z - a,
                    r.h - 22.,
                    0.,
                    with_alpha(color, if b == selected { 0.16 } else { 0.07 }),
                );
                let y = r.py((0.5 - gains[b] / 48.).clamp(0.03, 0.97));
                kit::line(
                    window,
                    &[
                        r.pt(freq_frac(e[b]), (0.5 - gains[b] / 48.).clamp(0., 1.)),
                        r.pt(freq_frac(e[b + 1]), (0.5 - gains[b] / 48.).clamp(0., 1.)),
                    ],
                    1.5,
                    with_alpha(color, 0.75),
                );
                // Band meter, −60…0 dB from the bottom.
                let db = levels[b];
                let frac = ((db + 60.) / 60.).clamp(0., 1.);
                let h = frac * (r.h - 46.);
                kit::bar(
                    window,
                    a + 3.,
                    r.bottom() - 20. - h,
                    (z - a - 6.).max(1.),
                    h,
                    2.,
                    with_alpha(color, 0.45),
                );
                kit::txt(
                    window,
                    cx,
                    names[b],
                    (a + z) / 2.,
                    r.y + 4.,
                    TEXT_XS,
                    pal.muted,
                    TextAlign::Center,
                );
                kit::txt(
                    window,
                    cx,
                    &kit::fmt_db(gains[b]),
                    (a + z) / 2.,
                    y - 13.,
                    TEXT_XS,
                    if b == selected { pal.text } else { pal.muted },
                    TextAlign::Center,
                );
                kit::dot(
                    window,
                    r.pt(
                        freq_frac((e[b] * e[b + 1]).sqrt()),
                        (0.5 - gains[b] / 48.).clamp(0.03, 0.97),
                    ),
                    6.,
                    if enabled { color } else { pal.muted },
                    (b == selected).then_some(pal.text),
                );
            }
            for fx in &lines {
                kit::vline(window, r, r.px(*fx), pal.text);
                kit::dot(window, r.pt(*fx, 0.5), 6., pal.raised, Some(pal.text));
            }
        },
    );

    let mut rows = div().flex().flex_col().gap(px(SP1));
    for (b, name) in names.iter().enumerate() {
        rows = rows.child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(
                    div()
                        .flex_none()
                        .w(px(8.))
                        .h(px(8.))
                        .rounded_full()
                        .bg(series(b)),
                )
                .child(label(
                    *name,
                    TEXT_XS,
                    if b == selected { pal.text } else { pal.muted },
                ))
                .child(kit::num_w(ws, dev, p::band(b as u32, p::GAIN), 54., cx))
                .child(kit::pick(dev, p::band(b as u32, p::MUTE), cx))
                .child(kit::pick(dev, p::band(b as u32, p::SOLO), cx))
                .child(kit::pick(dev, p::band(b as u32, p::DELTA), cx)),
        );
    }

    let globals = div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(kit::strip(
            ws,
            dev,
            &[p::XOVER_LOW, p::XOVER_HIGH, p::MODE],
            &[],
            cx,
        ))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, p::BYPASS, cx))
                .child(caption(if dev.choice(p::MODE) == 1 {
                    "LINEAR — LATENCY COMPENSATED"
                } else {
                    "MINIMUM PHASE — ZERO LATENCY"
                })),
        );

    kit::panel(if dev.big { 760. } else { 640. })
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(
            div()
                .flex_none()
                .flex()
                .gap(px(SP2))
                .child(rows)
                .child(div().flex_none().w(px(1.)).bg(border()))
                .child(globals),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_stay_sorted_and_apart() {
        assert_eq!(edges(200., 2_500.), [20., 200., 2_500., 20_000.]);
        assert_eq!(edges(900., 600.), [20., 600., 900., 20_000.]);
        let e = edges(200., 250.);
        assert!(e[2] > 240.);
    }
}
