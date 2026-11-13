//! `fx.amp`: the clipping stage's transfer curve (drag sideways = gain)
//! next to the tone section + cabinet response (drag the handle up and
//! down = Tone, or Treble in Amp mode), the mode's knobs below.

use super::super::kit::{self, Handle, PlotCfg, ax, freq_frac};
use super::super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::saturator::amp::{MODES, mode, mode_params, p, tone_db, transfer};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Response plot range (dB) around 0.
const RESP_DB: f32 = 24.;

/// Plot fraction of an output level of the transfer curve (±2).
pub fn curve_frac(y: f32) -> f32 {
    (0.5 - y / 4.4).clamp(0., 1.)
}

pub fn resp_frac(db: f32) -> f32 {
    (0.5 - db / (2. * RESP_DB)).clamp(0., 1.)
}

/// Where the tone handle sits on the response plot (frequency).
pub fn tone_handle_freq(m: usize) -> f32 {
    match m {
        mode::FUZZ => 800.,
        mode::SCREAMER => 3_000.,
        _ => 4_000.,
    }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let m = dev.choice(p::MODE).min(3);
    let pct = |id| dev.plain(id) / 100.;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let (gain, tone, bias) = (pct(p::GAIN), pct(p::TONE), pct(p::BIAS));
    let (bass, mid, treble) = (pct(p::BASS), pct(p::MID), pct(p::TREBLE));
    let cab = dev.choice(p::CAB).min(3);
    let enabled = dev.enabled;
    let clip = dev.m(2);

    let at = 0.3f32;
    let handles = vec![Handle {
        fx: (at + 1.) / 2.,
        fy: curve_frac(transfer(m, gain, bias, at)),
        x: ax(p::GAIN, 1.),
        index: 0,
        reset: vec![default(p::GAIN)],
        ..Default::default()
    }];
    let hp = (handles[0].fx, handles[0].fy);
    let curve = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        kit::vline(window, r, r.px(0.5), pal.grid_bar);
        kit::line(window, &[r.pt(0., curve_frac(-1.)), r.pt(1., curve_frac(1.))], 1., content(0.14));
        let color = if enabled { pal.accent } else { pal.muted };
        let pts = kit::sample_curve(r, 120, |fx| curve_frac(transfer(m, gain, bias, fx * 2. - 1.)));
        kit::line(window, &pts, 2., color);
        // Clipping activity: a bar along the left edge.
        let h = (clip / 1.5).clamp(0., 1.) * (r.h - 8.);
        kit::bar(window, r.x + 2., r.bottom() - 4. - h, 4., h, 2., with_alpha(pal.accent2, 0.7));
        kit::txt(window, cx, MODES[m], r.right() - 6., r.y + 3., TEXT_XS, pal.muted, TextAlign::Right);
        kit::dot(window, r.pt(hp.0, hp.1), 6., color, Some(pal.text));
    });

    let sr = dev.sample_rate.max(44_100.);
    let tone_id = if m == mode::AMP { p::TREBLE } else { p::TONE };
    let fh = tone_handle_freq(m);
    let handles = vec![Handle {
        fx: freq_frac(fh),
        fy: resp_frac(tone_db(m, tone, bass, mid, treble, cab, fh as f64, sr)),
        y: ax(tone_id, -1.),
        index: 1,
        reset: vec![default(tone_id)],
        ..Default::default()
    }];
    let th = (handles[0].fx, handles[0].fy);
    let cab_name = dev.text(p::CAB);
    let resp = kit::plot(ws, dev, PlotCfg { slot: 1, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::freq_grid(window, cx, r, true);
        kit::hline(window, r, r.py(resp_frac(0.)), pal.grid_bar);
        let color = if enabled { pal.accent } else { pal.muted };
        let pts = kit::sample_curve(r, 120, |fx| resp_frac(tone_db(m, tone, bass, mid, treble, cab, kit::frac_freq(fx) as f64, sr)));
        kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.08));
        kit::line(window, &pts, 2., color);
        kit::txt(window, cx, &format!("CAB {cab_name}"), r.right() - 6., r.y + 4., TEXT_XS, pal.muted, TextAlign::Right);
        kit::dot(window, r.pt(th.0, th.1), 6., series(1), Some(pal.text));
    });

    let mut knobs = vec![p::MODE];
    knobs.extend_from_slice(mode_params(m));
    kit::panel(if dev.big { 780. } else { 540. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(div().flex_none().w(px(if dev.big { 260. } else { 176. })).h_full().child(curve))
                .child(div().flex_1().min_w(px(0.)).h_full().child(resp)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(caption("CIRCUIT"))
                .child(kit::strip(ws, dev, &knobs, &[], cx))
                .child(div().w(px(SP2)))
                .child(kit::strip(ws, dev, &[p::CAB, p::OUTPUT, p::MIX], &[p::INPUT, p::OVERSAMPLING], cx)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_and_handles_fit_their_plots() {
        for m in 0..4 {
            for g in [0., 0.5, 1.] {
                for x in [-1., -0.3, 0., 0.3, 1.] {
                    let y = curve_frac(transfer(m, g, 0.5, x));
                    assert!((0. ..=1.).contains(&y));
                }
            }
            let f = tone_handle_freq(m);
            assert!((20. ..=20_000.).contains(&f));
        }
        assert_eq!(resp_frac(0.), 0.5);
    }
}
