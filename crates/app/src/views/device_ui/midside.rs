//! `fx.midside`: the mid/side field — mid on the vertical axis, side on the
//! horizontal, the width lens around it — plus M/S meters and the gain,
//! width and solo controls.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::midside::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, canvas, div, px};

/// Half-width of the stereo lens in plot fractions for a width percentage.
pub fn lens(width: f32) -> f32 {
    0.03 + (width / 200.).clamp(0., 1.) * 0.38
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let mid = dev.plain(p::MID);
    let side = dev.plain(p::SIDE);
    let width = dev.plain(p::WIDTH);
    let enabled = dev.enabled;
    let (fx, fy) = (
        (0.5 + side / 48.).clamp(0.04, 0.96),
        (0.5 - mid / 48.).clamp(0.04, 0.96),
    );
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![Handle {
        fx,
        fy,
        x: ax(p::SIDE, 1.),
        y: ax(p::MID, -1.),
        index: 0,
        reset: vec![default(p::MID), default(p::SIDE)],
        ..Default::default()
    }];
    let lw = lens(width);
    let field = kit::plot(
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
            kit::hline(window, r, r.py(0.5), pal.grid_bar);
            kit::vline(window, r, r.px(0.5), pal.grid_bar);
            let color = if enabled { pal.accent } else { pal.muted };
            // The width lens: narrow for mono, wide for a spread image.
            let diamond = [
                r.pt(0.5, 0.08),
                r.pt(0.5 + lw, 0.5),
                r.pt(0.5, 0.92),
                r.pt(0.5 - lw, 0.5),
            ];
            let mut outline = diamond.to_vec();
            outline.push(diamond[0]);
            crate::paint::polygon(window, &diamond, with_alpha(color, 0.1));
            kit::line(window, &outline, 1.2, with_alpha(color, 0.65));
            // L and R vectors lean out with the width.
            kit::line(
                window,
                &[r.pt(0.5, 0.5), r.pt(0.5 + lw * 0.75, 0.13)],
                1.5,
                with_alpha(series(0), 0.8),
            );
            kit::line(
                window,
                &[r.pt(0.5, 0.5), r.pt(0.5 - lw * 0.75, 0.13)],
                1.5,
                with_alpha(series(1), 0.8),
            );
            kit::txt(
                window,
                cx,
                "L",
                r.px(0.5 + lw * 0.75) + 2.,
                r.y + 3.,
                TEXT_XS,
                with_alpha(series(0), 0.9),
                TextAlign::Left,
            );
            kit::txt(
                window,
                cx,
                "R",
                r.px(0.5 - lw * 0.75) - 2.,
                r.y + 3.,
                TEXT_XS,
                with_alpha(series(1), 0.9),
                TextAlign::Right,
            );
            kit::txt(
                window,
                cx,
                "M",
                r.px(0.5) + 4.,
                r.y + 16.,
                TEXT_XS,
                pal.faint,
                TextAlign::Left,
            );
            kit::txt(
                window,
                cx,
                "S",
                r.right() - 6.,
                r.py(0.5) - 13.,
                TEXT_XS,
                pal.faint,
                TextAlign::Right,
            );
            kit::txt(
                window,
                cx,
                &format!("mid {mid:+.1}  side {side:+.1}  width {width:.0}%"),
                r.px(0.5),
                r.bottom() - 14.,
                TEXT_XS,
                pal.muted,
                TextAlign::Center,
            );
            kit::dot(window, r.pt(fx, fy), 7., color, Some(pal.text));
        },
    );

    let m = dev.m(0);
    let s = dev.m(1);
    let meters = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let pal = p();
            let r = Rect::of(b);
            for (i, (name, db)) in [("M", m), ("S", s)].iter().enumerate() {
                let x = r.x + 2. + i as f32 * 17.;
                let frac = ((db + 60.) / 60.).clamp(0., 1.);
                let h = (r.h - 16.) * frac;
                kit::bar(window, x, r.y, 12., r.h - 16., 2., pal.well);
                kit::bar(
                    window,
                    x,
                    r.y + (r.h - 16.) - h,
                    12.,
                    h,
                    2.,
                    if *db > -0.1 {
                        pal.meter_high
                    } else {
                        pal.meter_low
                    },
                );
                kit::txt(
                    window,
                    cx,
                    name,
                    x + 6.,
                    r.bottom() - 13.,
                    TEXT_XS,
                    pal.faint,
                    TextAlign::Center,
                );
            }
        },
    )
    .w(px(38.))
    .h_full()
    .flex_none();

    kit::panel(if dev.big { 540. } else { 440. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap(px(SP1))
                        .child(div().flex_1().min_h(px(0.)).child(field))
                        .child(caption("SIDE →   MID ↓")),
                )
                .child(meters),
        )
        .child(kit::strip(
            ws,
            dev,
            &[p::MID, p::SIDE, p::WIDTH],
            &[p::SOLO_MID, p::SOLO_SIDE],
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lens_opens_with_width() {
        assert!(lens(0.) < lens(100.) && lens(100.) < lens(200.));
        assert!(lens(200.) <= 0.42);
        assert!(lens(0.) >= 0.03);
    }
}
