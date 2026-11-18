//! `fx.freqshift`: the partials of a 220 Hz tone on a linear 0–4 kHz axis,
//! the original in grey and where the shifter puts them in colour. Every
//! partial moves by the same number of Hz, so the evenly spaced comb stays
//! evenly spaced but slides off the harmonic series; Ring mode makes both
//! sidebands, and a negative result folds back as a dim mirror image. With
//! Stereo Spread the right channel is drawn hanging from the baseline. The
//! bar along the bottom is Shift (drag it); the numbers follow the LFO live.

use super::kit::{self, Handle, PlotCfg, ax};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::freqshift::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Fundamental of the drawn tone and how many partials are shown.
const F0: f32 = 220.;
const PARTIALS: usize = 17;
/// Frequency at the right edge of the plot.
const F_MAX: f32 = 4000.;
/// Highest shift the control reaches, either way.
const SHIFT_MAX: f32 = 2000.;
/// Height of the tallest partial, as a fraction of the plot.
const REACH: f32 = 0.42;
/// Vertical position of the baseline and of the shift track.
const BASE: f32 = 0.56;
const TRACK: f32 = 0.92;

/// One drawn line of the spectrum: frequency in Hz (always positive) and
/// height 0..1, and whether it folded over zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub hz: f32,
    pub height: f32,
    pub folded: bool,
}

/// Where the partials of the tone land for a shift of `shift` Hz: one line
/// per partial, or two (up and down, half as tall) in ring mode.
pub fn lines(shift: f32, ring: bool) -> Vec<Line> {
    let mut out = Vec::new();
    for k in 1..=PARTIALS {
        let f = F0 * k as f32;
        let h = 1. / (k as f32).powf(0.7);
        let mut add = |hz: f32, height: f32| out.push(Line { hz: hz.abs(), height, folded: hz < 0. });
        if ring {
            add(f + shift, h * 0.5);
            add(f - shift, h * 0.5);
        } else {
            add(f + shift, h);
        }
    }
    out
}

/// The right channel's shift for a Stereo Spread of `spread` (0..1): the left
/// shift at 0, none at 50 %, the opposite at 100 %.
pub fn right_shift(shift: f32, spread: f32) -> f32 {
    shift * (1. - 2. * spread)
}

/// "+120 Hz" / "−1.20 kHz".
pub fn hz_text(hz: f32) -> String {
    if hz.abs() >= 1000. { format!("{:+.2} kHz", hz / 1000.) } else { format!("{hz:+.0} Hz") }
}

/// Fraction of the shift track for a shift in Hz.
fn track_frac(shift: f32) -> f32 {
    (0.5 + shift / (2. * SHIFT_MAX)).clamp(0., 1.)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let ring = dev.choice(p::MODE) == 1;
    let static_shift = dev.plain(p::SHIFT) + dev.plain(p::FINE);
    let spread = dev.plain(p::SPREAD) / 100.;
    let live = dev.meter.len() >= 5;
    // With the LFO running, the meter has the shift as it is right now.
    let (left, right) = if live { (dev.m(0), dev.m(1)) } else { (static_shift, right_shift(static_shift, spread)) };
    let enabled = dev.enabled;
    let norm = dev.norm(p::SHIFT);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![Handle { fx: 0.04 + 0.92 * norm, fy: TRACK, x: ax(p::SHIFT, 1. / 0.92), index: 0, reset: vec![default(p::SHIFT), default(p::FINE)], ..Default::default() }];
    let title = if ring { format!("ring · carrier {:.0} Hz", left.abs()) } else { format!("{} · {}", hz_text(left), if left >= 0. { "up" } else { "down" }) };
    let split = spread > 0.001;

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        let base = r.py(BASE);
        let x_of = |hz: f32| r.x + r.w * (hz / F_MAX).clamp(0., 1.);
        for k in 1..4 {
            let hz = 1000. * k as f32;
            kit::vline(window, r, x_of(hz), pal.grid_beat);
            kit::txt(window, cx, &format!("{k}k"), x_of(hz) + 3., r.py(0.66), TEXT_XS, pal.faint, TextAlign::Left);
        }
        kit::hline(window, r, base, pal.grid_bar);
        let reach = r.h * REACH;
        // The original series.
        for l in lines(0., false) {
            kit::bar(window, x_of(l.hz) - 0.5, base - reach * l.height, 1., reach * l.height, 0., content(0.22));
        }
        // The left channel's lines above the baseline, the right one's below.
        for l in lines(left, ring) {
            let c = with_alpha(colour, if l.folded { 0.4 } else { 0.9 });
            kit::bar(window, x_of(l.hz) - 1., base - reach * l.height, 2., reach * l.height, 1., c);
        }
        if split {
            for l in lines(right, ring) {
                let c = with_alpha(if enabled { series(1) } else { pal.faint }, if l.folded { 0.35 } else { 0.8 });
                kit::bar(window, x_of(l.hz) - 1., base, 2., reach * 0.6 * l.height, 1., c);
            }
        }
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        if split {
            kit::txt(window, cx, &format!("L {}  R {}", hz_text(left), hz_text(right)), r.right() - 6., r.y + 4., TEXT_XS, pal.muted, TextAlign::Right);
        }
        // The shift track: centre is no shift.
        let (tx0, tx1) = (r.px(0.04), r.px(0.96));
        let (mid, at) = (r.px(0.5), r.px(0.04 + 0.92 * norm));
        kit::bar(window, tx0, r.py(TRACK) - 2., tx1 - tx0, 4., 2., content(0.08));
        kit::bar(window, mid - 0.5, r.py(TRACK) - 5., 1., 10., 0., pal.grid_bar);
        kit::bar(window, mid.min(at), r.py(TRACK) - 2., (at - mid).abs(), 4., 2., with_alpha(colour, 0.7));
        kit::dot(window, r.pt(0.04 + 0.92 * norm, TRACK), 5.5, colour, Some(pal.text));
        // The live (LFO-moved) shift as a tick on the same track.
        if live {
            let x = tx0 + (tx1 - tx0) * track_frac(left);
            kit::bar(window, x - 1., r.py(TRACK) - 6., 2., 12., 1., pal.accent2);
        }
    });

    let mut row = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1)).child(kit::pick(dev, p::MODE, cx));
    if dev.big || dev.plain(p::LFO_DEPTH) > 0. {
        row = row.child(kit::pick(dev, p::LFO_SHAPE, cx));
    }
    let primary = [p::SHIFT, p::FINE, p::LFO_RATE, p::LFO_DEPTH, p::FEEDBACK, p::SPREAD, p::MIX];
    kit::panel(if dev.big { 600. } else { 440. })
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(row.child(kit::strip(ws, dev, &primary, &[p::OUTPUT], cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shift_moves_every_partial_by_the_same_hz() {
        let flat = lines(0., false);
        let moved = lines(100., false);
        assert_eq!(flat.len(), moved.len());
        for (a, b) in flat.iter().zip(&moved) {
            assert!((b.hz - a.hz - 100.).abs() < 1e-3 && b.height == a.height && !b.folded);
        }
    }

    #[test]
    fn ring_mode_draws_both_sidebands_and_negative_results_fold() {
        let ring = lines(100., true);
        assert_eq!(ring.len(), 2 * PARTIALS);
        assert!((ring[0].hz - 320.).abs() < 1e-3 && (ring[1].hz - 120.).abs() < 1e-3);
        let down = lines(-500., false);
        assert!(down[0].folded && (down[0].hz - 280.).abs() < 1e-3 && down[1].folded && !down[2].folded);
    }

    #[test]
    fn spread_goes_from_the_same_shift_to_the_opposite_one() {
        assert_eq!(right_shift(100., 0.), 100.);
        assert_eq!(right_shift(100., 0.5), 0.);
        assert_eq!(right_shift(100., 1.), -100.);
    }

    #[test]
    fn hz_text_switches_to_khz() {
        assert_eq!(hz_text(120.), "+120 Hz");
        assert_eq!(hz_text(-1200.), "-1.20 kHz");
        assert!((track_frac(0.) - 0.5).abs() < 1e-6 && track_frac(5000.) == 1.);
    }
}
