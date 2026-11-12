//! `fx.clipper`: the transfer curve (drag the handle sideways = drive, up and
//! down = ceiling), the clipping-reduction / input / output meters, and the
//! curve, lift, delta and oversampling controls.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::{db_to_gain, gain_to_db};
use aitrack_stock::fx::clipper::{curve_sample, p};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, canvas, div, px};

/// Output level (dB) of the clipper for a steady input level (dB).
pub fn transfer(x_db: f32, drive_db: f32, ceiling_db: f32, soft: bool) -> f32 {
    let y = curve_sample(
        db_to_gain(x_db),
        db_to_gain(drive_db),
        db_to_gain(ceiling_db),
        soft,
    );
    gain_to_db(y)
}

/// Plot fraction of an input level, −24…+24 dB.
fn in_frac(db: f32) -> f32 {
    ((db + 24.) / 48.).clamp(0., 1.)
}

/// Plot fraction of an output level, −24…+24 dB.
fn out_frac(db: f32) -> f32 {
    ((24. - db) / 48.).clamp(0., 1.)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let drive = dev.plain(p::DRIVE);
    let ceiling = dev.plain(p::CEILING);
    let soft = dev.choice(p::CURVE) == 0;
    let enabled = dev.enabled;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let threshold = ceiling - drive;
    let handles = vec![Handle {
        fx: in_frac(threshold).clamp(0.04, 0.96),
        fy: out_frac(ceiling).clamp(0.04, 0.96),
        x: ax(p::DRIVE, -2.),
        y: ax(p::CEILING, -4.),
        index: 0,
        reset: vec![default(p::DRIVE), default(p::CEILING)],
        ..Default::default()
    }];
    let curve = kit::plot(
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
            kit::db_grid(window, cx, r, -24., 24., 12., true);
            kit::line(window, &[r.pt(0., 1.), r.pt(1., 0.)], 1., content(0.14));
            if (0. ..=1.).contains(&in_frac(threshold)) {
                kit::vline(
                    window,
                    r,
                    r.px(in_frac(threshold)),
                    with_alpha(pal.accent2, 0.45),
                );
            }
            let color = if enabled { pal.accent } else { pal.muted };
            let pts = kit::sample_curve(r, 96, |fx| {
                out_frac(transfer(-24. + 48. * fx, drive, ceiling, soft))
            });
            kit::fill_to(window, &pts, r.bottom(), with_alpha(color, 0.1));
            kit::line(window, &pts, 2., color);
            kit::txt(
                window,
                cx,
                &format!("{threshold:+.1} dB"),
                r.px(in_frac(threshold)) + 4.,
                r.y + 3.,
                TEXT_XS,
                pal.muted,
                TextAlign::Left,
            );
            kit::txt(
                window,
                cx,
                "IN",
                r.x + 6.,
                r.bottom() - 14.,
                TEXT_XS,
                pal.faint,
                TextAlign::Left,
            );
            kit::txt(
                window,
                cx,
                "OUT",
                r.right() - 6.,
                r.y + 3.,
                TEXT_XS,
                pal.faint,
                TextAlign::Right,
            );
            kit::dot(
                window,
                r.pt(
                    in_frac(threshold).clamp(0.04, 0.96),
                    out_frac(ceiling).clamp(0.04, 0.96),
                ),
                7.,
                color,
                Some(pal.text),
            );
        },
    );

    let gr = dev.m(0);
    let input = dev.m(1);
    let output = dev.m(2);
    let meters = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let pal = p();
            let r = Rect::of(b);
            let bars = [
                ("GR", -gr, 0., 24.),
                ("IN", input, -60., 24.),
                ("OUT", output, -60., 0.),
            ];
            for (i, (name, value, lo, hi)) in bars.iter().enumerate() {
                let x = r.x + 2. + i as f32 * 17.;
                let frac = ((value - lo) / (hi - lo)).clamp(0., 1.);
                let h = (r.h - 16.) * frac;
                kit::bar(window, x, r.y, 12., r.h - 16., 2., pal.well);
                kit::bar(
                    window,
                    x,
                    if *name == "GR" {
                        r.y
                    } else {
                        r.y + (r.h - 16.) - h
                    },
                    12.,
                    h,
                    2.,
                    if *name == "GR" {
                        pal.accent2
                    } else if *value > -0.1 {
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
    .w(px(54.))
    .h_full()
    .flex_none();

    kit::panel(if dev.big { 560. } else { 440. })
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
                        .child(div().flex_1().min_h(px(0.)).child(curve))
                        .child(caption("← DRIVE   CEILING ↑")),
                )
                .child(meters),
        )
        .child(kit::strip(
            ws,
            dev,
            &[p::CEILING, p::DRIVE, p::CURVE, p::LIFT],
            &[p::DELTA, p::OVERSAMPLING],
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_is_bounded_by_the_ceiling_and_rises() {
        for soft in [false, true] {
            let mut last = f32::MIN;
            for i in 0..=96 {
                let y = transfer(-24. + 48. * i as f32 / 96., 12., -3., soft);
                assert!(y <= -3. + 1e-3, "{soft}: {y}");
                assert!(y >= last - 1e-4, "{soft}: {y} < {last}");
                last = y;
            }
        }
    }

    #[test]
    fn the_handle_sits_inside_the_plot() {
        for (drive, ceiling) in [(0., 0.), (24., -12.), (6., -0.3)] {
            assert!((0. ..=1.).contains(&(drive / 24.)));
            assert!((0. ..=1.).contains(&(-ceiling / 12.)));
        }
    }
}
