//! `fx.filter`: the response curve with one node riding on it (drag =
//! cutoff / resonance), the LFO and envelope depths as shaded ranges, and
//! the cutoff the plugin is at right now. The curve helpers are shared with
//! the synth and sampler filter sections.

use super::kit::{self, Handle, PlotCfg, Rect, ax, frac_freq, freq_frac};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::response::svf_cascade_db;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode, butterworth_q};
use aitrack_stock::fx::filter::{p, resonance_q};
use gpui::{AnyElement, App, Context, Hsla, IntoElement, ParentElement, Pixels, Point, Styled, TextAlign, Window, div, px};

/// Top and bottom of the filter plots, dB.
pub const DB_TOP: f32 = 24.;
pub const DB_BOTTOM: f32 = -48.;

pub fn db_frac(db: f32) -> f32 {
    ((DB_TOP - db) / (DB_TOP - DB_BOTTOM)).clamp(0., 1.)
}

/// The sections `fx.filter` runs for these settings (one, or two for 24 dB/oct).
pub fn sections(mode: usize, slope24: bool, cutoff: f32, resonance: f32, sample_rate: f64) -> Vec<SvfCoeffs> {
    let mode = match mode {
        1 => SvfMode::Highpass,
        2 => SvfMode::Bandpass,
        3 => SvfMode::Notch,
        4 => SvfMode::Peak,
        _ => SvfMode::Lowpass,
    };
    let q = resonance_q(resonance);
    if slope24 {
        // A Butterworth pair; resonance lifts the second section.
        let (q1, q2) = match mode {
            SvfMode::Lowpass | SvfMode::Highpass => (butterworth_q(4, 0), butterworth_q(4, 1) * q / std::f32::consts::FRAC_1_SQRT_2),
            _ => (q, q),
        };
        vec![SvfCoeffs::new(mode, cutoff, q1, 0., sample_rate), SvfCoeffs::new(mode, cutoff, q2, 0., sample_rate)]
    } else {
        vec![SvfCoeffs::new(mode, cutoff, q, 0., sample_rate)]
    }
}

/// The curve of a filter across a plot's width.
pub fn curve(r: Rect, sections: &[SvfCoeffs], sample_rate: f64) -> Vec<Point<Pixels>> {
    let n = ((r.w / 3.) as usize).clamp(32, 300);
    kit::sample_curve(r, n, |fx| db_frac(svf_cascade_db(sections, frac_freq(fx) as f64, sample_rate) as f32))
}

/// Grid, filled curve and outline of a filter response.
pub fn paint_response(window: &mut Window, cx: &mut App, r: Rect, sections: &[SvfCoeffs], sample_rate: f64, color: Hsla, labels: bool) {
    kit::db_grid(window, cx, r, DB_BOTTOM, DB_TOP, 12., labels);
    kit::freq_grid(window, cx, r, labels);
    let pts = curve(r, sections, sample_rate);
    kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.16));
    kit::line(window, &pts, 2., color);
}

/// Where the cutoff node sits: on the curve at the cutoff frequency.
pub fn node(sections: &[SvfCoeffs], cutoff: f32, sample_rate: f64) -> (f32, f32) {
    (freq_frac(cutoff), db_frac(svf_cascade_db(sections, cutoff as f64, sample_rate) as f32).clamp(0.04, 0.96))
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let (cutoff, resonance) = (dev.plain(p::CUTOFF), dev.plain(p::RESONANCE));
    let secs = sections(dev.choice(p::TYPE), dev.choice(p::SLOPE) == 1, cutoff, resonance, dev.sample_rate);
    let (fx, fy) = node(&secs, cutoff, dev.sample_rate);
    let reset = [p::CUTOFF, p::RESONANCE].map(|id| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()))).to_vec();
    let handles = vec![Handle { fx, fy, x: ax(p::CUTOFF, 1.), y: ax(p::RESONANCE, -1.1), alt: ax(p::RESONANCE, -1.5), index: 0, reset }];
    let (lfo, env) = (dev.plain(p::LFO_DEPTH), dev.plain(p::ENV_AMOUNT));
    let live = [dev.m(0), dev.m(1)];
    let (sr, enabled) = (dev.sample_rate, dev.enabled);
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        // Modulation ranges behind the curve: LFO both ways, envelope one way.
        if lfo.abs() > 0.01 {
            let (a, b) = (r.px(freq_frac(cutoff * 2f32.powf(-lfo.abs())).clamp(0., 1.)), r.px(freq_frac(cutoff * 2f32.powf(lfo.abs())).clamp(0., 1.)));
            kit::bar(window, a, r.y, b - a, r.h, 0., with_alpha(series(1), 0.13));
            kit::txt(window, cx, "LFO", a + 4., r.y + 4., TEXT_XS, with_alpha(series(1), 0.9), TextAlign::Left);
        }
        if env.abs() > 0.01 {
            let (a, b) = (r.px(freq_frac(cutoff)), r.px(freq_frac(cutoff * 2f32.powf(env)).clamp(0., 1.)));
            kit::bar(window, a.min(b), r.y + r.h * 0.55, (a - b).abs(), r.h * 0.45, 0., with_alpha(series(3), 0.16));
            kit::txt(window, cx, "Env", a.min(b) + 4., r.bottom() - 28., TEXT_XS, with_alpha(series(3), 0.9), TextAlign::Left);
        }
        paint_response(window, cx, r, &secs, sr, if enabled { pal.accent } else { pal.muted }, true);
        // Where the modulated cutoff is right now (left and right channel).
        for hz in live {
            if hz > 10. && (hz / cutoff - 1.).abs() > 0.01 {
                kit::vline(window, r, r.px(freq_frac(hz).clamp(0., 1.)), with_alpha(pal.accent2, 0.8));
            }
        }
        kit::dot(window, r.pt(fx, fy), 8., pal.accent, Some(pal.text));
    });
    let rate = if dev.on(p::LFO_SYNC) { p::LFO_DIV } else { p::LFO_RATE };
    kit::panel(if dev.big { 640. } else { 440. })
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(div().flex_none().flex().items_center().gap(px(SP1)).child(kit::pick(dev, p::TYPE, cx)).child(kit::pick(dev, p::SLOPE, cx)).child(kit::pick(dev, p::LFO_SYNC, cx)))
        .child(kit::strip(dev, &[p::DRIVE, p::LFO_DEPTH, rate, p::ENV_AMOUNT, p::MIX], &[p::LFO_SHAPE, p::LFO_STEREO, p::ENV_ATTACK, p::ENV_RELEASE, p::OUTPUT], cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;
    use aitrack_stock::test_util::{process_stereo, rms, set_plain, sine};

    #[test]
    fn the_drawn_curve_is_what_the_filter_does() {
        for (mode, slope, cutoff, reso) in [(0., 0., 800., 15.), (0., 1., 1_200., 45.), (1., 1., 300., 30.), (2., 0., 2_000., 50.), (3., 0., 1_000., 20.)] {
            let secs = sections(mode as usize, slope == 1., cutoff, reso, 48_000.);
            for f in [cutoff * 0.25, cutoff * 0.7, cutoff * 1.6, cutoff * 4.] {
                let mut filter = aitrack_stock::create("fx.filter").unwrap();
                set_plain(filter.as_mut(), p::TYPE, mode);
                set_plain(filter.as_mut(), p::SLOPE, slope);
                set_plain(filter.as_mut(), p::CUTOFF, cutoff);
                set_plain(filter.as_mut(), p::RESONANCE, reso);
                let n = 24_000;
                let (l, _) = process_stereo(filter.as_mut(), sine(f, 0.1), n);
                let x: Vec<f32> = (0..n).map(|i| sine(f, 0.1)(i).0).collect();
                let measured = 20. * (rms(&l[n / 2..]) / rms(&x[n / 2..])).log10();
                let drawn = svf_cascade_db(&secs, f as f64, 48_000.) as f32;
                if drawn > -55. {
                    assert!((drawn - measured).abs() < 0.3, "mode {mode} slope {slope} at {f:.0} Hz: drawn {drawn:.2}, measured {measured:.2}");
                }
            }
        }
    }

    #[test]
    fn the_node_rides_the_curve_at_the_cutoff() {
        let d = dev("fx.filter", &[(p::CUTOFF, 1_000.), (p::RESONANCE, 60.)]);
        let secs = sections(d.choice(p::TYPE), false, d.plain(p::CUTOFF), d.plain(p::RESONANCE), 48_000.);
        let (fx, fy) = node(&secs, 1_000., 48_000.);
        assert!((fx - freq_frac(1_000.)).abs() < 1e-6);
        // A resonant low-pass peaks above 0 dB at the cutoff.
        assert!(fy < db_frac(0.));
        assert_eq!(sections(0, true, 500., 20., 48_000.).len(), 2);
        assert_eq!(db_frac(DB_TOP), 0.);
        assert_eq!(db_frac(DB_BOTTOM), 1.);
    }
}
