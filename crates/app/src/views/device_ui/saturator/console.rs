//! `fx.console`: the mode's curve (Channel sin, Buss asin, Glue's console
//! amp — drag the handle sideways = drive) next to the glue compressor's
//! static curve with its gain-reduction meter (drag up and down = Glue).

use super::super::kit::{self, Handle, PlotCfg, ax};
use super::super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::db_to_gain;
use aitrack_stock::fx::saturator::console::{MODES, curve, glue_gr, mode, p};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Plot fraction of a curve output (±1.6).
pub fn curve_frac(y: f32) -> f32 {
    (0.5 - y / 3.4).clamp(0., 1.)
}

/// Plot fraction of a level on the compressor's −48…0 dB axes.
pub fn level_frac(db: f32) -> f32 {
    ((db + 48.) / 48.).clamp(0., 1.)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let m = dev.choice(p::MODE).min(2);
    let g = db_to_gain(dev.plain(p::DRIVE));
    let glue = dev.plain(p::GLUE) / 100.;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let enabled = dev.enabled;
    let gr = dev.m(2).max(0.);

    // Curve over input −1.5…1.5.
    let at = 0.8f32;
    let handles = vec![Handle { fx: (at / 1.5 + 1.) / 2., fy: curve_frac(curve(m, at, g)), x: ax(p::DRIVE, 1.), index: 0, reset: vec![default(p::DRIVE)], ..Default::default() }];
    let hp = (handles[0].fx, handles[0].fy);
    let shape = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        kit::vline(window, r, r.px(0.5), pal.grid_bar);
        kit::line(window, &[r.pt(0., curve_frac(-1.5)), r.pt(1., curve_frac(1.5))], 1., content(0.14));
        let color = if enabled { pal.accent } else { pal.muted };
        let pts = kit::sample_curve(r, 120, |fx| curve_frac(curve(m, (fx * 2. - 1.) * 1.5, g)));
        kit::line(window, &pts, 2., color);
        kit::txt(window, cx, MODES[m], r.right() - 6., r.y + 3., TEXT_XS, pal.muted, TextAlign::Right);
        kit::dot(window, r.pt(hp.0, hp.1), 6., color, Some(pal.text));
    });

    // Glue: static curve, input level → output level.
    let is_glue = m == mode::GLUE;
    let handles = if is_glue {
        let t = -18.;
        vec![Handle { fx: level_frac(t), fy: 1. - level_frac(t - glue_gr(t, glue)), y: ax(p::GLUE, 1.), index: 1, reset: vec![default(p::GLUE)], ..Default::default() }]
    } else {
        Vec::new()
    };
    let gh = handles.first().map(|h| (h.fx, h.fy));
    let comp = kit::plot(ws, dev, PlotCfg { slot: 1, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::db_grid(window, cx, r, -48., 0., 12., true);
        kit::line(window, &[r.pt(0., 1.), r.pt(1., 0.)], 1., content(0.14));
        let color = if enabled && is_glue { pal.accent } else { pal.muted };
        let pts = kit::sample_curve(r, 96, |fx| {
            let x = -48. + 48. * fx;
            1. - level_frac(x - if is_glue { glue_gr(x, glue) } else { 0. })
        });
        kit::line(window, &pts, 2., color);
        // Gain reduction meter on the right edge (0…12 dB down from the top).
        let h = (gr / 12.).clamp(0., 1.) * (r.h - 8.);
        kit::bar(window, r.right() - 8., r.y + 4., 4., h, 2., with_alpha(pal.accent2, 0.8));
        kit::txt(window, cx, if is_glue { "GLUE" } else { "GLUE OFF" }, r.x + 6., r.y + 3., TEXT_XS, pal.muted, TextAlign::Left);
        if let Some((x, y)) = gh {
            kit::dot(window, r.pt(x, y), 6., color, Some(pal.text));
        }
    });

    let mut main = vec![p::MODE, p::DRIVE];
    if is_glue {
        main.push(p::GLUE);
    }
    main.push(p::IRON);
    kit::panel(if dev.big { 720. } else { 480. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(div().flex_1().min_w(px(0.)).h_full().child(shape))
                .child(div().flex_1().min_w(px(0.)).h_full().child(comp)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(caption("CONSOLE"))
                .child(kit::strip(ws, dev, &main, &[], cx))
                .child(div().w(px(SP2)))
                .child(kit::strip(ws, dev, &[p::OUTPUT, p::MIX], &[p::OVERSAMPLING], cx))
                .child(kit::readout("GR", format!("{gr:.1} dB"), p().accent2)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_fit_their_plots() {
        for m in 0..3 {
            for g in [db_to_gain(-12.), 1., db_to_gain(18.)] {
                for x in [-1.5, -0.5, 0., 0.7, 1.5] {
                    assert!((0. ..=1.).contains(&curve_frac(curve(m, x, g))));
                }
            }
        }
        assert_eq!(level_frac(0.), 1.);
        assert_eq!(level_frac(-60.), 0.);
    }
}
