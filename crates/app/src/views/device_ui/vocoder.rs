//! `fx.vocoder`: one bar per band at its place on a log frequency axis, as
//! tall as the modulator's level in that band right now. Formant is drawn as
//! outline caps where those levels land on the synthesis side (the voice's
//! shape slid up or down the axis). The dots on the bottom axis are the
//! lowest and highest band centres (drag them) and the bar along the top is
//! Formant (drag it).

use super::kit::{self, Handle, PlotCfg, ax};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::vocoder::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Frequency range of the plot's log axis.
const AX_LO: f32 = 40.;
const AX_HI: f32 = 20_000.;
/// Fractions of the plot: the bars' baseline, the band-range track, the formant track.
const BASE: f32 = 0.84;
const RANGE_Y: f32 = 0.93;
const FORMANT_Y: f32 = 0.1;
/// Share of the plot width the formant bar uses, centred.
const GAUGE_SPAN: f32 = 0.9;
/// Longest a bar gets, as a fraction of the plot.
const REACH: f32 = 0.64;

/// Horizontal fraction of a frequency on the plot.
pub fn axis_frac(hz: f32) -> f32 {
    ((hz.max(1.) / AX_LO).ln() / (AX_HI / AX_LO).ln()).clamp(0., 1.)
}

/// Centre frequencies of `n` bands spaced evenly in log frequency from `lo` to `hi`.
pub fn centres(n: usize, lo: f32, hi: f32) -> Vec<f32> {
    let n = n.max(2);
    (0..n).map(|k| lo * (hi / lo).powf(k as f32 / (n - 1) as f32)).collect()
}

/// How far a log-scaled parameter's normalised value moves when its handle crosses the whole axis.
fn span_of(dev: &Dev, id: u32) -> f32 {
    dev.info(id).map_or(1., |i| (AX_HI / AX_LO).ln() / (i.max / i.min).ln())
}

/// "D3" for a frequency in Hz.
pub fn note_name(hz: f32) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let key = (69. + 12. * (hz.max(1.) / 440.).log2()).round() as i32;
    format!("{}{}", NAMES[key.rem_euclid(12) as usize], key.div_euclid(12) - 1)
}

/// What the carrier is, as a line of text.
fn carrier_text(dev: &Dev, tracked: f32) -> String {
    let chord = dev.text(p::CHORD);
    let key = aitrack_stock::common::key_to_hz(dev.plain(p::PITCH).round());
    match dev.choice(p::CARRIER) {
        0 => "carrier: sidechain input (saw while none is connected)".to_owned(),
        1 => "carrier: noise".to_owned(),
        2 => format!("carrier: saw {} · {chord}", note_name(key)),
        _ if tracked > 0. => format!("carrier: voice pitch {tracked:.0} Hz {} · {chord}", note_name(tracked)),
        _ => format!("carrier: follows the voice · {chord}"),
    }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let n = dev.plain(p::BANDS).round() as usize;
    let (lo, hi) = (dev.plain(p::LOW), dev.plain(p::HIGH));
    let formant = dev.plain(p::FORMANT);
    let live = dev.meter.len() >= 4 + n;
    let tracked = dev.m(1);
    let levels: Vec<f32> = (0..n).map(|k| if live { dev.m(4 + k) } else { 0. }).collect();
    let sibilance = dev.m(3);
    let enabled = dev.enabled;
    let formant_norm = dev.norm(p::FORMANT);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![
        Handle { fx: axis_frac(lo), fy: RANGE_Y, x: ax(p::LOW, span_of(dev, p::LOW)), index: 0, reset: vec![default(p::LOW)], ..Default::default() },
        Handle { fx: axis_frac(hi), fy: RANGE_Y, x: ax(p::HIGH, span_of(dev, p::HIGH)), index: 1, reset: vec![default(p::HIGH)], ..Default::default() },
        Handle { fx: (1. - GAUGE_SPAN) / 2. + GAUGE_SPAN * formant_norm, fy: FORMANT_Y, x: ax(p::FORMANT, 1. / GAUGE_SPAN), index: 2, reset: vec![default(p::FORMANT)], ..Default::default() },
    ];
    let title = carrier_text(dev, tracked);
    let freqs = centres(n, lo, hi);

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        let base = r.py(BASE);
        for (f, name) in [(100., "100"), (1_000., "1k"), (10_000., "10k")] {
            kit::vline(window, r, r.px(axis_frac(f)), pal.grid_beat);
            kit::txt(window, cx, name, r.px(axis_frac(f)) + 3., base + 2., TEXT_XS, pal.faint, TextAlign::Left);
        }
        kit::hline(window, r, base, pal.grid_bar);
        // Outside the band range nothing is analysed.
        let (x_lo, x_hi) = (r.px(axis_frac(lo)), r.px(axis_frac(hi)));
        kit::bar(window, r.x, r.y, (x_lo - r.x).max(0.), r.h, 0., with_alpha(pal.base, 0.35));
        kit::bar(window, x_hi, r.y, (r.right() - x_hi).max(0.), r.h, 0., with_alpha(pal.base, 0.35));
        let reach = r.h * REACH;
        let width = ((x_hi - x_lo) / n as f32 * 0.7).clamp(2., 22.);
        let shift = 2f32.powf(formant / 12.);
        for (k, f) in freqs.iter().enumerate() {
            let h = if live { reach * levels[k].clamp(0., 1.) } else { 3. };
            let x = r.px(axis_frac(*f));
            kit::bar(window, x - width / 2., base - h, width, h.max(1.), 1.5, with_alpha(colour, if live { 0.75 } else { 0.25 }));
            if live && formant.abs() > 0.05 {
                let gx = r.px(axis_frac(*f * shift));
                kit::bar(window, gx - width / 2., base - h - 1., width, 2., 1., with_alpha(pal.accent2, 0.9));
            }
        }
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, &format!("{n} bands · formant {formant:+.1} st"), r.x + 6., r.py(FORMANT_Y) + 10., TEXT_XS, pal.muted, TextAlign::Left);
        // Sibilance: the level of the unvoiced path.
        if live {
            kit::bar(window, r.right() - 12., base - 40., 5., 40., 2., content(0.08));
            kit::bar(window, r.right() - 12., base - 40. * sibilance.clamp(0., 1.), 5., 40. * sibilance.clamp(0., 1.), 2., with_alpha(series(1), 0.8));
        }
        // Formant gauge on top.
        let (left, right) = (r.px((1. - GAUGE_SPAN) / 2.), r.px((1. + GAUGE_SPAN) / 2.));
        let (mid, at) = (r.px(0.5), r.px((1. - GAUGE_SPAN) / 2. + GAUGE_SPAN * formant_norm));
        kit::bar(window, left, r.py(FORMANT_Y) - 2., right - left, 4., 2., content(0.08));
        kit::bar(window, mid - 0.5, r.py(FORMANT_Y) - 5., 1., 10., 0., pal.grid_bar);
        kit::bar(window, mid.min(at), r.py(FORMANT_Y) - 2., (at - mid).abs(), 4., 2., with_alpha(colour, 0.7));
        kit::dot(window, r.pt((1. - GAUGE_SPAN) / 2. + GAUGE_SPAN * formant_norm, FORMANT_Y), 5.5, colour, Some(pal.text));
        // The band range on the bottom axis.
        kit::bar(window, x_lo, r.py(RANGE_Y) - 1., (x_hi - x_lo).max(1.), 2., 1., with_alpha(colour, 0.5));
        for f in [lo, hi] {
            kit::dot(window, r.pt(axis_frac(f), RANGE_Y), 5.5, colour, Some(pal.text));
        }
    });

    let mode = dev.choice(p::CARRIER);
    let mut row = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1)).child(kit::pick(dev, p::CARRIER, cx));
    if mode >= 2 {
        row = row.child(kit::pick(dev, p::CHORD, cx));
    }
    let mut primary = vec![p::BANDS, p::FORMANT, p::ATTACK, p::RELEASE, p::UNVOICED];
    if mode == 2 {
        primary.push(p::PITCH);
    }
    if mode >= 2 {
        primary.push(p::DETUNE);
    }
    if mode == 3 {
        primary.push(p::TRANSPOSE);
    }
    primary.push(p::MIX);
    kit::panel(if dev.big { 640. } else { 480. })
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(row.child(kit::strip(ws, dev, &primary, &[p::LOW, p::HIGH, p::BANDWIDTH, p::OUTPUT], cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_spaced_evenly_in_log_frequency() {
        let c = centres(5, 100., 1600.);
        assert!((c[0] - 100.).abs() < 1e-3 && (c[4] - 1600.).abs() < 0.1);
        for w in c.windows(2) {
            assert!((w[1] / w[0] - 2.).abs() < 1e-3);
        }
    }

    #[test]
    fn the_axis_maps_its_ends_to_the_plot_edges() {
        assert_eq!(axis_frac(AX_LO), 0.);
        assert_eq!(axis_frac(AX_HI), 1.);
        assert!(axis_frac(1000.) > axis_frac(100.));
    }

    #[test]
    fn notes_are_named_by_the_nearest_key() {
        assert_eq!(note_name(440.), "A4");
        assert_eq!(note_name(130.81), "C3");
        assert_eq!(note_name(147.), "D3");
    }
}
