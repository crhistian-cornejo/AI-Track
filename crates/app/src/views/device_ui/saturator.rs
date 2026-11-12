//! `fx.saturator`: each band's transfer shape (drag a curve's handle =
//! drive) next to the bands laid out over frequency, where the crossovers
//! are draggable lines and each band shows its level.

use super::kit::{self, Handle, PlotCfg, ax, freq_frac};
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::saturator::{STYLES, p};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Handle indices of the two crossovers (bands are 0..3).
const XOVER: usize = 8;

/// The curve of a style at drive-scaled input `u` (unity small-signal gain).
pub fn shape(style: usize, u: f32, bias: f32, bits: f32) -> f32 {
    match style {
        0 => u.tanh(),
        1 => ((u + bias).tanh() - bias.tanh()) / (1. - bias.tanh().powi(2)),
        2 => u.clamp(-1., 1.),
        3 => u.sin(),
        _ => {
            let steps = 2f32.powf(bits.clamp(1., 16.) - 1.);
            (u.clamp(-1., 1.) * steps).round() / steps
        }
    }
}

/// Band edges in Hz for `bands` active bands (the plugin sorts the crossovers).
pub fn edges(bands: usize, low: f32, high: f32) -> Vec<f32> {
    let (lo, hi) = (low.min(high), low.max(high));
    match bands {
        1 => vec![kit::F_LO, kit::F_HI],
        2 => vec![kit::F_LO, lo, kit::F_HI],
        _ => vec![kit::F_LO, lo, hi.max(lo * 1.05), kit::F_HI],
    }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let nb = dev.choice(p::BANDS) + 1;
    let selected = if dev.selected < nb { dev.selected } else { 0 };
    let band = |b: usize, x: u32| p::band(b as u32, x);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    // (style, linear drive, bias, bits) per band.
    let shapes: Vec<(usize, f32, f32, f32)> = (0..nb)
        .map(|b| (dev.choice(band(b, p::STYLE)), 10f32.powf(dev.plain(band(b, p::DRIVE)) / 20.), dev.plain(band(b, p::BIAS)) / 100. * 0.6, dev.plain(band(b, p::BITS))))
        .collect();
    let at = 0.35f32;
    let curve_y = |s: &(usize, f32, f32, f32), x: f32| 0.5 - shape(s.0, s.1 * x, s.2, s.3).clamp(-1.15, 1.15) / 2.4;
    let handles: Vec<Handle> = shapes
        .iter()
        .enumerate()
        .map(|(b, s)| Handle { fx: (at + 1.) / 2., fy: curve_y(s, at), y: ax(band(b, p::DRIVE), -1.), index: b, reset: vec![default(band(b, p::DRIVE))], ..Default::default() })
        .collect();
    let (shapes2, enabled) = (shapes.clone(), dev.enabled);
    let transfer = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, _| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        kit::vline(window, r, r.px(0.5), pal.grid_bar);
        kit::line(window, &[r.pt(0., 0.5 + 1. / 2.4), r.pt(1., 0.5 - 1. / 2.4)], 1., content(0.14));
        for (b, s) in shapes2.iter().enumerate() {
            let color = if enabled { series(b) } else { pal.muted };
            let pts = kit::sample_curve(r, 96, |fx| curve_y(s, fx * 2. - 1.));
            kit::line(window, &pts, if b == selected { 2.5 } else { 1.5 }, with_alpha(color, if b == selected { 1. } else { 0.7 }));
            kit::dot(window, r.pt((at + 1.) / 2., curve_y(s, at)), 6., color, (b == selected).then_some(pal.text));
        }
    });

    // Bands over frequency.
    let e = edges(nb, dev.plain(p::XOVER_LOW), dev.plain(p::XOVER_HIGH));
    let mut handles = Vec::new();
    for (i, id) in [p::XOVER_LOW, p::XOVER_HIGH].into_iter().enumerate().take(nb - 1) {
        let span = dev.info(id).map_or(1., kit::log_span);
        handles.push(Handle { fx: freq_frac(dev.plain(id)), fy: 0.5, x: ax(id, span), index: XOVER + i, reset: vec![default(id)], ..Default::default() });
    }
    let lines: Vec<f32> = handles.iter().map(|h| h.fx).collect();
    let levels: Vec<f32> = (0..nb).map(|b| dev.m(2 + b)).collect();
    let labels: Vec<String> = (0..nb).map(|b| format!("{} {:.0} dB", STYLES.get(shapes[b].0).copied().unwrap_or(""), dev.plain(band(b, p::DRIVE)))).collect();
    let spectrum = kit::plot(ws, dev, PlotCfg { slot: 1, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::freq_grid(window, cx, r, true);
        for b in 0..nb {
            let (a, z) = (r.px(freq_frac(e[b])), r.px(freq_frac(e[b + 1])));
            let color = series(b);
            kit::bar(window, a, r.y, z - a, r.h, 0., with_alpha(color, if b == selected { 0.14 } else { 0.07 }));
            // Level in the band, −48…0 dB from the bottom.
            let db = if levels[b] > 1e-4 { 20. * levels[b].log10() } else { -120. };
            let h = ((db + 48.) / 48.).clamp(0., 1.) * (r.h - 30.);
            kit::bar(window, a + 3., r.bottom() - 14. - h, (z - a - 6.).max(1.), h, 2., with_alpha(color, 0.55));
            kit::txt(window, cx, &labels[b], (a + z) / 2., r.y + 4., TEXT_XS, pal.muted, TextAlign::Center);
        }
        for fx in &lines {
            kit::vline(window, r, r.px(*fx), pal.text);
            kit::dot(window, r.pt(*fx, 0.5), 6., pal.raised, Some(pal.text));
        }
    });

    let b = selected;
    let style = dev.choice(band(b, p::STYLE));
    let mut band_ids = vec![band(b, p::STYLE), band(b, p::DRIVE), band(b, p::LEVEL)];
    match style {
        1 => band_ids.push(band(b, p::BIAS)),
        4 => band_ids.extend([band(b, p::BITS), band(b, p::CRUSH_RATE)]),
        _ => {}
    }
    kit::panel(if dev.big { 760. } else { 520. })
        .child(div().flex_1().min_h(px(0.)).flex().gap(px(SP2)).child(div().flex_none().w(px(if dev.big { 240. } else { 168. })).h_full().child(transfer)).child(div().flex_1().min_w(px(0.)).h_full().child(spectrum)))
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(div().size(px(8.)).rounded_full().bg(series(b)))
                .child(caption(["LOW", "MID", "HIGH"][if nb == 1 { 1 } else if nb == 2 { b * 2 } else { b }]))
                .child(kit::strip(dev, &band_ids, &[], cx))
                .child(div().w(px(SP2)))
                .child(kit::strip(dev, &[p::BANDS, p::MIX, p::OUTPUT, p::AUTO_GAIN], &[p::TONE, p::OVERSAMPLING], cx)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_unity_for_small_signals_and_bounded() {
        for style in 0..4 {
            let small = shape(style, 0.01, 0.2, 8.);
            assert!((small / 0.01 - 1.).abs() < 0.03, "style {style}: {small}");
            for u in [-40., -2., 0.5, 3., 40.] {
                assert!(shape(style, u, 0.2, 8.).abs() <= 1.5);
            }
        }
        // The crusher quantises to 2^(bits-1) steps per side.
        assert_eq!(shape(4, 0.3, 0., 2.), 0.5);
        assert_eq!(shape(4, 5., 0., 8.), 1.);
    }

    #[test]
    fn band_edges_follow_the_band_count() {
        assert_eq!(edges(1, 200., 2500.), vec![20., 20_000.]);
        assert_eq!(edges(2, 200., 2500.), vec![20., 200., 20_000.]);
        assert_eq!(edges(3, 200., 2500.), vec![20., 200., 2500., 20_000.]);
        // Crossed crossovers are sorted and kept apart, as the plugin does.
        assert_eq!(edges(3, 900., 600.), vec![20., 600., 900., 20_000.]);
    }
}
