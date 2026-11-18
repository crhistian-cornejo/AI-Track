//! `fx.pitchshift`: the transposition on a −24…+24 semitone ruler (drag the
//! handle sideways for semitones, up and down for cents), what it does to
//! A4, and below it the grain the plugin crossfades: one Hann window with
//! the two read heads riding it, their heights always adding up to one.

use super::kit::{self, Handle, PlotCfg, ax};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::pitchshift::{latency_samples, p};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Semitones the ruler spans each way.
const SPAN: f32 = 24.;
/// Height of the grain plot.
const GRAIN_H: f32 = 72.;
/// Fraction of the ruler's height its baseline sits at.
const BASE: f32 = 0.64;

/// Horizontal fraction of a transposition on the ruler.
pub fn semi_frac(semitones: f32) -> f32 {
    ((semitones + SPAN) / (2. * SPAN)).clamp(0., 1.)
}

/// Frequency ratio of a transposition.
pub fn ratio(semitones: f32) -> f32 {
    (semitones / 12.).exp2()
}

/// The Hann window the heads fade under, at `x` in 0..1.
pub fn hann(x: f32) -> f32 {
    let s = (std::f32::consts::PI * x.rem_euclid(1.)).sin();
    s * s
}

/// "+7 st +12 ct" with the zero parts left out.
pub fn shift_text(semitones: f32, cents: f32) -> String {
    match (semitones.round() as i32, cents.round() as i32) {
        (0, 0) => "no shift".to_owned(),
        (s, 0) => format!("{s:+} st"),
        (0, c) => format!("{c:+} ct"),
        (s, c) => format!("{s:+} st {c:+} ct"),
    }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let (semi, cents) = (dev.plain(p::SEMITONES).round(), dev.plain(p::CENTS));
    let total = semi + cents / 100.;
    let grain = dev.plain(p::GRAIN);
    let latency_ms = latency_samples(grain, dev.sample_rate) / dev.sample_rate as f32 * 1000.;
    let align = dev.on(p::ALIGN);
    let live = dev.meter.len() >= 8;
    let (heads, peaks) = ([dev.m(1), dev.m(2)], [dev.m(6), dev.m(7)]);
    let enabled = dev.enabled;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![Handle {
        fx: semi_frac(total),
        fy: 0.5 - 0.3 * (cents / 100.),
        x: ax(p::SEMITONES, 1.),
        y: ax(p::CENTS, -1. / 0.6),
        index: 0,
        reset: vec![default(p::SEMITONES), default(p::CENTS)],
        ..Default::default()
    }];
    let title = format!("{} · A4 440 Hz becomes {:.0} Hz", shift_text(semi, cents), 440. * ratio(total));

    let ruler = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        let base = r.py(BASE);
        // One tick per semitone, taller on octaves.
        for s in -24..=24 {
            let x = r.px(semi_frac(s as f32));
            let octave = s % 12 == 0;
            let h = if octave { 16. } else if s % 12 == 7 || s % 12 == -5 { 10. } else { 6. };
            kit::bar(window, x - 0.5, base, 1., h, 0., if octave { pal.grid_bar } else { pal.grid_beat });
            if octave {
                let text = if s == 0 { "0".to_owned() } else { format!("{s:+}") };
                kit::txt(window, cx, &text, x, base + 18., TEXT_XS, pal.faint, TextAlign::Center);
            }
        }
        kit::hline(window, r, base, pal.grid_bar);
        // From the dry pitch to the shifted one.
        let (x0, x1) = (r.px(0.5), r.px(semi_frac(total)));
        kit::bar(window, x0.min(x1), base - 6., (x1 - x0).abs().max(1.), 6., 3., with_alpha(colour, 0.45));
        kit::bar(window, x0 - 1., base - 22., 2., 22., 1., content(0.4));
        kit::txt(window, cx, "dry", x0, base - 36., TEXT_XS, pal.faint, TextAlign::Center);
        kit::vline(window, r, x1, with_alpha(colour, 0.35));
        kit::dot(window, r.pt(semi_frac(total), 0.5 - 0.3 * (cents / 100.)), 7., colour, Some(pal.text));
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, &format!("×{:.3}", ratio(total)), r.right() - 6., r.y + 4., TEXT_XS, pal.muted, TextAlign::Right);
        if live {
            for (i, v) in peaks.iter().enumerate() {
                let h = 34. * v.clamp(0., 1.);
                kit::bar(window, r.right() - 14. + 6. * i as f32, r.bottom() - 6. - h, 4., h, 2., with_alpha(series(i), 0.7));
            }
        }
    });

    let grain_plot = kit::plot(ws, dev, PlotCfg { slot: 1, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let y = |v: f32| 0.84 - 0.64 * v;
        kit::hline(window, r, r.py(y(1.)), pal.grid_beat);
        kit::hline(window, r, r.py(y(0.)), pal.grid_bar);
        let a = kit::sample_curve(r, 96, |fx| y(hann(fx)));
        let b = kit::sample_curve(r, 96, |fx| y(hann(fx + 0.5)));
        kit::fill_to(window, &a, r.py(y(0.)), with_alpha(series(0), 0.10));
        kit::line(window, &b, 1., with_alpha(series(1), 0.5));
        kit::line(window, &a, 1.5, with_alpha(series(0), 0.85));
        if live {
            for (i, ph) in heads.iter().enumerate() {
                kit::dot(window, r.pt(*ph, y(hann(*ph))), 4., if enabled { series(i) } else { pal.muted }, None);
            }
        }
        let text = format!("grain {grain:.0} ms · latency {latency_ms:.0} ms · align {}", if align { "on" } else { "off" });
        kit::txt(window, cx, &text, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
    });

    let primary = [p::SEMITONES, p::CENTS, p::GRAIN, p::FEEDBACK, p::MIX];
    kit::panel(if dev.big { 600. } else { 440. })
        .child(div().flex_1().min_h(px(0.)).child(ruler))
        .child(div().flex_none().h(px(GRAIN_H)).child(grain_plot))
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, p::ALIGN, cx))
                .child(kit::strip(ws, dev, &primary, &[p::OUTPUT], cx)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_maps_the_range_onto_the_plot() {
        assert_eq!(semi_frac(-24.), 0.);
        assert_eq!(semi_frac(0.), 0.5);
        assert_eq!(semi_frac(24.), 1.);
        assert_eq!(semi_frac(60.), 1.);
        assert!((ratio(12.) - 2.).abs() < 1e-6 && (ratio(-12.) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn heads_half_a_grain_apart_add_up_to_one() {
        for i in 0..=20 {
            let x = i as f32 / 20.;
            assert!((hann(x) + hann(x + 0.5) - 1.).abs() < 1e-5, "{x}");
        }
    }

    #[test]
    fn shift_text_leaves_out_zero_parts() {
        assert_eq!(shift_text(0., 0.), "no shift");
        assert_eq!(shift_text(7., 0.), "+7 st");
        assert_eq!(shift_text(0., -30.), "-30 ct");
        assert_eq!(shift_text(-12., 14.), "-12 st +14 ct");
    }
}
