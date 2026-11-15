//! `fx.crusher`: two pictures of what the plugin does. On the left the
//! quantiser's transfer curve (input across, output up): its steps follow
//! Bits, its bend Shape and its offset DC Shift, with the levels the plugin
//! is holding right now as lines. On the right a short stretch of a sine
//! sampled and held at the chosen rate, the steps' width being the rate
//! (jitter moves their edges). Bits is the bar on the transfer plot's right
//! edge and Rate the one along the wave plot's bottom: drag them.

use super::kit::{self, F_HI, Handle, PlotCfg, ax, freq_frac};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::crusher::{p, transfer};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};
use std::f32::consts::TAU;

/// The share of a plot's height or width a gauge bar uses, centred.
const GAUGE_SPAN: f32 = 0.9;
/// Hold periods the wave plot shows, and sine cycles in them.
const PERIODS: f32 = 8.;
const CYCLES: f32 = 2.;
/// Peak of the drawn sine.
const AMPLITUDE: f32 = 0.9;
/// How far from the middle of a plot a full-scale level is drawn.
const REACH: f32 = 0.44;

/// The plot fraction of a normalised value on a gauge.
fn gauge_frac(norm: f32) -> f32 {
    (1. - GAUGE_SPAN) / 2. + GAUGE_SPAN * norm.clamp(0., 1.)
}

/// What the Rate parameter comes to at the host's rate: the rate the held
/// samples are taken at and how many samples each is held for.
pub fn hold_of(rate_hz: f32, host_hz: f32) -> (f32, f32) {
    let eff = rate_hz.min(host_hz).max(1.);
    (eff, (host_hz / eff).max(1.))
}

/// Highest frequency the held samples can carry: half the rate they are
/// taken at. What lies above it folds back down as aliasing.
pub fn nyquist(eff_rate: f32) -> f32 {
    eff_rate / 2.
}

/// A frequency as text: "8 kHz", "950 Hz".
pub fn hz_text(hz: f32) -> String {
    if hz >= 1000. { format!("{:.1} kHz", hz / 1000.) } else { format!("{hz:.0} Hz") }
}

/// Edges of the held steps over `PERIODS` periods, in fractions of the plot:
/// evenly spaced, each moved by up to `jitter` (0..1) of half a period,
/// always in order. A fixed sequence keeps the picture steady.
pub fn step_edges(jitter: f32) -> Vec<f32> {
    let mut seed = 0x2545_f491_u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / 8_388_608.0 - 1.0
    };
    let mut out = vec![0.];
    for k in 1..PERIODS as usize {
        let at = k as f32 + 0.5 * jitter.clamp(0., 1.) * noise();
        out.push((at / PERIODS).max(out[out.len() - 1] + 1e-3));
    }
    out.push(1.);
    out
}

/// The held-and-quantised level of the sine at plot fraction `fx`.
fn crushed(edges: &[f32], fx: f32, bits: f32, shape: f32, dc: f32) -> f32 {
    let start = edges.iter().rev().find(|e| **e <= fx).copied().unwrap_or(0.);
    transfer(AMPLITUDE * (TAU * CYCLES * start).sin(), bits, shape, dc)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let (bits, shape, dc) = (dev.plain(p::BITS), dev.plain(p::SHAPE), dev.plain(p::DC_SHIFT));
    let rate = dev.plain(p::RATE);
    let jitter = dev.plain(p::JITTER) / 100.;
    let host = dev.sample_rate as f32;
    let (eff_rate, hold) = hold_of(rate, host);
    let passes = rate >= host && jitter == 0.;
    let live = dev.meter.len() >= 2;
    let (hold_l, hold_r) = (dev.m(0), dev.m(1));
    let enabled = dev.enabled;
    let (bits_norm, rate_norm) = (dev.norm(p::BITS), dev.norm(p::RATE));
    let levels = if bits >= 15.95 { "unquantised".to_owned() } else { format!("{} levels", 2f32.powf(bits.round().clamp(1., 16.)) as u32) };
    let bits_title = format!("{} bits · {levels}", ((bits * 10.).round() / 10.));
    let nyq = nyquist(eff_rate);
    let rate_title = if passes {
        "at the host rate: nothing is held".to_owned()
    } else {
        format!("{:.1} kHz · held {hold:.1} samples", eff_rate / 1000.)
    };
    let dc_norm = dev.norm(p::DC_SHIFT);

    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let bits_handle = Handle { fx: 0.96, fy: gauge_frac(1. - dev.norm(p::BITS)), y: ax(p::BITS, -1. / GAUGE_SPAN), index: 0, reset: vec![default(p::BITS)], ..Default::default() };
    let rate_handle = Handle { fx: gauge_frac(dev.norm(p::RATE)), fy: 0.94, x: ax(p::RATE, 1. / GAUGE_SPAN), index: 1, reset: vec![default(p::RATE)], ..Default::default() };

    let dc_handle = Handle { fx: gauge_frac(dc_norm), fy: 0.94, x: ax(p::DC_SHIFT, 1. / GAUGE_SPAN), index: 2, reset: vec![default(p::DC_SHIFT)], ..Default::default() };
    let curve_plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles: vec![bits_handle, dc_handle], keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        for q in [0.25, 0.75] {
            kit::vline(window, r, r.px(q), pal.grid_beat);
            kit::hline(window, r, r.py(q), pal.grid_beat);
        }
        kit::vline(window, r, r.px(0.5), pal.grid_bar);
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        // The unprocessed line, then the curve with its steps.
        kit::line(window, &[r.pt(0., 0.5 + REACH), r.pt(1., 0.5 - REACH)], 1., content(0.18));
        let n = ((r.w * 2.) as usize).clamp(128, 900);
        let curve = kit::sample_curve(r, n, |fx| 0.5 - REACH * transfer(2. * fx - 1., bits, shape, dc).clamp(-1.1, 1.1));
        kit::fill_to(window, &curve, r.py(0.5), with_alpha(colour, 0.12));
        kit::line(window, &curve, 1.5, colour);
        if live {
            for (v, c) in [(hold_l, series(0)), (hold_r, series(1))] {
                kit::hline(window, r, r.py(0.5 - REACH * v.clamp(-1., 1.)), with_alpha(c, 0.7));
            }
        }
        // Bits: a bar on the right edge.
        let (top, bottom) = (r.py(gauge_frac(0.)), r.py(gauge_frac(1.)));
        kit::bar(window, r.px(0.96) - 2., top, 4., bottom - top, 2., content(0.08));
        let now = r.py(gauge_frac(1. - bits_norm));
        kit::bar(window, r.px(0.96) - 2., now, 4., bottom - now, 2., with_alpha(pal.accent, 0.7));
        kit::dot(window, r.pt(0.96, gauge_frac(1. - bits_norm)), 5.5, pal.accent, Some(pal.text));
        // DC Shift: a bar along the bottom edge, from the middle (no offset).
        let (left, right) = (r.px(gauge_frac(0.)), r.px(gauge_frac(1.)));
        let (mid, at) = (r.px(0.5), r.px(gauge_frac(dc_norm)));
        kit::bar(window, left, r.py(0.94) - 2., right - left, 4., 2., content(0.08));
        kit::bar(window, mid, r.py(0.94) - 3., 1., 6., 0., pal.grid_bar);
        kit::bar(window, mid.min(at), r.py(0.94) - 2., (at - mid).abs(), 4., 2., with_alpha(pal.accent, 0.7));
        kit::dot(window, r.pt(gauge_frac(dc_norm), 0.94), 5.5, pal.accent, Some(pal.text));
        kit::txt(window, cx, &bits_title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, &format!("dc {:+.0}%", dc * 1.), r.x + 6., r.py(0.94) - 18., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "in", r.right() - 24., r.py(0.94) - 18., TEXT_XS, pal.faint, TextAlign::Right);
        kit::txt(window, cx, "out", r.x + 6., r.y + 18., TEXT_XS, pal.faint, TextAlign::Left);
    });

    let wave_plot = kit::plot(ws, dev, PlotCfg { slot: 1, handles: vec![rate_handle], keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        let n = ((r.w / 2.) as usize).clamp(64, 480);
        let input = kit::sample_curve(r, n, |fx| 0.5 - REACH * AMPLITUDE * (TAU * CYCLES * fx).sin());
        kit::line(window, &input, 1., content(0.22));
        let edges = if passes { (0..=n).map(|i| i as f32 / n as f32).collect() } else { step_edges(jitter) };
        let mut steps = Vec::new();
        for w in edges.windows(2) {
            let y = 0.5 - REACH * crushed(&edges, w[0], bits, shape, dc).clamp(-1.1, 1.1);
            steps.push(r.pt(w[0], y));
            steps.push(r.pt(w[1], y));
        }
        kit::fill_to(window, &steps, r.py(0.5), with_alpha(colour, 0.10));
        kit::line(window, &steps, 1.5, colour);
        // Rate: a bar along the bottom edge.
        let (left, right) = (r.px(gauge_frac(0.)), r.px(gauge_frac(1.)));
        kit::bar(window, left, r.py(0.94) - 2., right - left, 4., 2., content(0.08));
        let now = r.px(gauge_frac(rate_norm));
        kit::bar(window, left, r.py(0.94) - 2., now - left, 4., 2., with_alpha(pal.accent, 0.7));
        kit::dot(window, r.pt(gauge_frac(rate_norm), 0.94), 5.5, pal.accent, Some(pal.text));
        kit::txt(window, cx, &rate_title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        // Nyquist: the audio band as a strip, the part above half the rate
        // marked as the part that folds back.
        let (sy, sh) = (r.py(0.66), 6.);
        kit::bar(window, left - 6., sy - 20., right - left + 12., sh + 40., 4., with_alpha(pal.base, 0.7));
        let nx = left + (right - left) * freq_frac(nyq.min(F_HI)).clamp(0., 1.);
        kit::bar(window, left, sy, nx - left, sh, 2., with_alpha(colour, 0.35));
        if nx < right - 1. {
            kit::bar(window, nx, sy, right - nx, sh, 2., with_alpha(pal.record, 0.35));
        }
        for (f, name) in [(100., "100"), (1_000., "1k"), (10_000., "10k")] {
            let x = left + (right - left) * freq_frac(f);
            kit::bar(window, x, sy + sh, 1., 3., 0., pal.grid_bar);
            kit::txt(window, cx, name, x, sy + sh + 3., TEXT_XS, pal.faint, TextAlign::Center);
        }
        kit::bar(window, nx - 1., sy - 3., 2., sh + 6., 1., pal.text);
        let label = format!("Nyquist {}", hz_text(nyq));
        kit::txt(window, cx, &label, nx.min(right - 4.), sy - 16., TEXT_XS, pal.muted, if nx > left + (right - left) * 0.7 { TextAlign::Right } else { TextAlign::Left });
    });

    let frame = |plot| div().min_h(px(0.)).h_full().rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(plot);
    let mut picks = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1));
    for id in [p::PRE, p::POST, p::KEEP_DC, p::STEREO_JITTER] {
        picks = picks.child(kit::pick(dev, id, cx));
    }
    let mut ids = vec![p::RATE, p::JITTER, p::BITS, p::SHAPE, p::DC_SHIFT];
    if dev.on(p::POST) || dev.big {
        ids.push(p::POST_OCT);
    }
    ids.extend([p::MIX, p::OUTPUT]);

    kit::panel(if dev.big { 640. } else { 460. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(div().flex_none().w(px(if dev.big { 232. } else { 188. })).child(frame(curve_plot)))
                .child(div().flex_1().min_w(px(0.)).child(frame(wave_plot))),
        )
        .child(picks)
        .child(kit::strip(ws, dev, &ids, &[], cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn bits_make_steps_in_the_transfer_curve() {
        // 3 bits: 4 levels per side, so 8 distinct outputs over −1..1 at most.
        let mut seen: Vec<f32> = (0..=200).map(|i| transfer(i as f32 / 100. - 1., 3., 0., 0.)).collect();
        seen.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        assert!(seen.len() <= 9, "{}", seen.len());
        // 16 bits is the identity, and Shape bends it.
        assert_eq!(transfer(0.3, 16., 0., 0.), 0.3);
        assert_ne!(transfer(0.3, 4., 80., 0.), transfer(0.3, 4., 0., 0.));
        // DC Shift moves where the steps fall.
        assert_ne!(transfer(0.12, 3., 0., 60.), transfer(0.12, 3., 0., 0.));
    }

    #[test]
    fn the_rate_shows_what_is_held() {
        assert_eq!(hold_of(16_000., 48_000.), (16_000., 3.));
        // At or above the host rate nothing is held.
        assert_eq!(hold_of(48_000., 44_100.), (44_100., 1.));
        // Jitter keeps the edges in order and inside the plot, 0 keeps them even.
        for j in [0., 0.5, 1.] {
            let e = step_edges(j);
            assert_eq!((e.len(), e[0], *e.last().unwrap()), (PERIODS as usize + 1, 0., 1.));
            assert!(e.windows(2).all(|w| w[1] > w[0]), "{j}");
        }
        assert!((step_edges(0.)[3] - 3. / PERIODS).abs() < 1e-6);
        assert_ne!(step_edges(1.)[3], step_edges(0.)[3]);
    }

    #[test]
    fn nyquist_is_half_the_rate_that_is_held() {
        assert_eq!(nyquist(16_000.), 8_000.);
        assert_eq!(nyquist(hold_of(48_000., 44_100.).0), 22_050.);
        assert_eq!((hz_text(8_000.), hz_text(950.)), ("8.0 kHz".to_owned(), "950 Hz".to_owned()));
        // The strip puts it between the ends of the audio band.
        assert!(freq_frac(8_000.) > 0.5 && freq_frac(nyquist(200.)) < 0.3);
    }

    #[test]
    fn the_wave_is_held_between_edges_and_quantised() {
        let edges = step_edges(0.);
        // Two points inside one hold give the same level.
        assert_eq!(crushed(&edges, 0.30, 4., 0., 0.), crushed(&edges, 0.30 + 0.01, 4., 0., 0.));
        // 1 bit leaves two levels.
        let levels: std::collections::BTreeSet<i32> = (0..100).map(|i| (crushed(&edges, i as f32 / 100., 1., 0., 0.) * 1000.).round() as i32).collect();
        assert!(levels.len() <= 3, "{levels:?}");
    }

    #[test]
    fn the_editor_asks_for_the_params_the_plugin_has() {
        let d = dev("fx.crusher", &[(p::BITS, 5.), (p::RATE, 8_000.)]);
        assert_eq!(d.plain(p::BITS), 5.);
        for id in [p::RATE, p::JITTER, p::PRE, p::POST, p::POST_OCT, p::BITS, p::SHAPE, p::DC_SHIFT, p::KEEP_DC, p::STEREO_JITTER, p::MIX, p::OUTPUT] {
            assert!(d.info(id).is_some(), "{id}");
        }
        assert!(gauge_frac(0.) > 0. && gauge_frac(1.) < 1.);
    }
}
