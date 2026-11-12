//! `fx.utility`: the stereo field as a fan (drag its handle sideways = pan,
//! up/down = width), output meters with the phase correlation, and the
//! phase / mono / swap switches as buttons.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::utility::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Pixels, Point, Styled, TextAlign, canvas, div, point, px};

/// The outline of the stereo fan in plot fractions: apex at the bottom
/// centre, opening with the width and leaning with the pan.
pub fn fan(width: f32, pan: f32, mono: bool) -> Vec<(f32, f32)> {
    let half = if mono { 0.02 } else { (width / 200.).clamp(0., 1.) * 80f32.to_radians() + 0.02 };
    let lean = (pan / 100.).clamp(-1., 1.) * 35f32.to_radians();
    let apex = (0.5, 0.94);
    let mut pts = vec![apex];
    for i in 0..=24 {
        let a = lean - half + 2. * half * i as f32 / 24.;
        pts.push((apex.0 + 0.46 * a.sin(), apex.1 - 0.84 * a.cos()));
    }
    pts
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (width, pan, mono) = (dev.plain(p::WIDTH), dev.plain(p::PAN), dev.on(p::MONO));
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let (hx, hy) = (0.5 + pan / 100. * 0.4, 0.86 - width / 200. * 0.72);
    let handles = vec![Handle { fx: hx, fy: hy, x: ax(p::PAN, 1. / 0.8), y: ax(p::WIDTH, -1. / 0.72), index: 0, reset: vec![default(p::PAN), default(p::WIDTH)], ..Default::default() }];
    let (enabled, muted) = (dev.enabled, dev.on(p::MUTE));
    let field = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::vline(window, r, r.px(0.5), pal.grid_bar);
        for a in [-45f32, 45.] {
            let rad = a.to_radians();
            kit::line(window, &[r.pt(0.5, 0.94), r.pt(0.5 + 0.46 * rad.sin(), 0.94 - 0.84 * rad.cos())], 1., pal.grid_beat);
        }
        kit::txt(window, cx, "L", r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "R", r.right() - 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Right);
        let color = if enabled && !muted { pal.accent } else { pal.muted };
        let pts: Vec<Point<Pixels>> = fan(width, pan, mono).into_iter().map(|(x, y)| r.pt(x, y)).collect();
        crate::paint::polygon(window, &pts, with_alpha(color, 0.25));
        let mut outline = pts.clone();
        outline.push(pts[0]);
        kit::line(window, &outline, 1.5, color);
        kit::txt(window, cx, &format!("width {width:.0}%  pan {pan:+.0}"), r.px(0.5), r.bottom() - 14., TEXT_XS, pal.muted, TextAlign::Center);
        kit::dot(window, r.pt(hx, hy), 7., pal.accent, Some(pal.text));
    });

    // Output peaks and correlation.
    let (peaks, corr) = ([dev.m(0), dev.m(1)], dev.m(2));
    let meters = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let pal = p();
            let r = Rect::of(b);
            for (i, v) in peaks.iter().enumerate() {
                let db = if *v > 1e-5 { 20. * v.log10() } else { -120. };
                let frac = ((db + 60.) / 66.).clamp(0., 1.);
                let x = r.x + i as f32 * 12.;
                kit::bar(window, x, r.y, 9., r.h - 16., 2., pal.well);
                kit::bar(window, x, r.y + (r.h - 16.) * (1. - frac), 9., (r.h - 16.) * frac, 2., if db > -0.1 { pal.meter_high } else { pal.meter_low });
                kit::txt(window, cx, if i == 0 { "L" } else { "R" }, x + 4.5, r.bottom() - 13., TEXT_XS, pal.faint, TextAlign::Center);
            }
            // Correlation: −1 (out of phase) at the bottom, +1 at the top.
            let x = r.x + 32.;
            kit::bar(window, x, r.y, 6., r.h - 16., 2., pal.well);
            let mid = r.y + (r.h - 16.) / 2.;
            let h = corr.clamp(-1., 1.) * (r.h - 16.) / 2.;
            kit::bar(window, x, if h > 0. { mid - h } else { mid }, 6., h.abs(), 2., if corr < 0. { pal.record } else { pal.accent2 });
            window.paint_quad(crate::paint::fill_rect(crate::paint::rect(x - 2., mid, 10., 1.), pal.muted));
            let _ = point(px(0.), px(0.));
            kit::txt(window, cx, "φ", x + 3., r.bottom() - 13., TEXT_XS, pal.faint, TextAlign::Center);
        },
    )
    .w(px(44.))
    .h_full()
    .flex_none();

    let switches = div()
        .flex_1()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(caption("PHASE"))
        .child(div().flex().flex_wrap().gap(px(SP1)).child(kit::pick(dev, p::INVERT_L, cx)).child(kit::pick(dev, p::INVERT_R, cx)).child(kit::pick(dev, p::SWAP, cx)))
        .child(caption("CHANNELS"))
        .child(div().flex().flex_wrap().gap(px(SP1)).child(kit::pick(dev, p::MODE, cx)).child(kit::pick(dev, p::MONO, cx)).child(kit::pick(dev, p::BASS_MONO, cx)))
        .child(div().flex().flex_wrap().gap(px(SP1)).child(kit::pick(dev, p::DC_BLOCK, cx)).child(kit::pick(dev, p::MUTE, cx)))
        .child(label_gap())
        .child(kit::strip(dev, &[p::GAIN, p::PAN, p::WIDTH], &[p::BASS_FREQ], cx));
    let _ = pal;
    kit::panel(if dev.big { 520. } else { 420. })
        .child(div().flex_1().min_h(px(0.)).flex().gap(px(SP2)).child(div().flex_none().w(px(if dev.big { 240. } else { 176. })).h_full().child(field)).child(meters).child(switches))
        .into_any_element()
}

fn label_gap() -> gpui::Div {
    div().flex_1()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fan_opens_with_width_and_leans_with_pan() {
        let spread = |pts: &[(f32, f32)]| pts.iter().map(|p| p.0).fold(f32::MIN, f32::max) - pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        let (narrow, normal, wide) = (fan(0., 0., false), fan(100., 0., false), fan(200., 0., false));
        assert!(spread(&narrow) < spread(&normal) && spread(&normal) < spread(&wide));
        assert!(spread(&fan(200., 0., true)) < 0.05, "mono collapses the field");
        // Centred: symmetric around the middle. Panned right: it leans right.
        let centre = |pts: &[(f32, f32)]| pts[1..].iter().map(|p| p.0).sum::<f32>() / (pts.len() - 1) as f32;
        assert!((centre(&normal) - 0.5).abs() < 1e-4);
        assert!(centre(&fan(100., 100., false)) > 0.6);
        assert!(centre(&fan(100., -100., false)) < 0.4);
        assert!(wide.iter().all(|p| (0. ..=1.).contains(&p.0) && (0. ..=1.).contains(&p.1)));
    }
}
