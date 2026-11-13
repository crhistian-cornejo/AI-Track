//! `instrument.kick808`: 808 amplitude & pitch-sweep contour visualizer with
//! draggable Decay / Sweep handles + chromatic & drive strips.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, caption};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::instrument::kick808::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, prelude::FluentBuilder, px};

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let w = if dev.big { 420. } else { 312. };
    let tune = dev.plain(p::TUNE);
    let sweep = dev.plain(p::SWEEP);
    let sweep_ms = dev.plain(p::SWEEP_DECAY);
    let decay_ms = dev.plain(p::DECAY);
    let drive = dev.plain(p::DRIVE) / 100.;
    let active = dev.m(0) > 0.5;
    let live_env = dev.m(3).clamp(0., 1.);
    let enabled = dev.enabled;
    let color = if enabled { pal.accent } else { pal.faint };

    // Time window matches up to 2.5s or current decay * 1.4.
    let window_ms = (decay_ms * 1.35).clamp(250., 3500.);
    let fx = (decay_ms / window_ms).clamp(0.08, 0.92);
    let fy = (1.0 - sweep / 36.0).clamp(0.08, 0.92);
    let reset = [p::DECAY, p::SWEEP]
        .map(|id| (id, dev.info(id).map_or(0.5, |i| i.default_normalized())))
        .to_vec();
    let handles = vec![Handle { fx, fy, x: ax(p::DECAY, 1.), y: ax(p::SWEEP, -1.), alt: ax(p::DRIVE, -1.), index: 0, reset }];

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r: Rect, window, cx| {
        let pal = p();
        // Horizontal reference lines.
        kit::hline(window, r, r.py(0.5), border());
        kit::hline(window, r, r.py(0.85), with_alpha(pal.faint, 0.25));

        let n = ((r.w / 3.) as usize).clamp(48, 220);
        // Amplitude envelope + warm filled body.
        let amp_pts = kit::sample_curve(r, n, |t| {
            let ms = t * window_ms;
            let e = (-ms / decay_ms.max(10.)).exp();
            1.0 - e * 0.88
        });
        kit::fill_to(window, &amp_pts, r.bottom(), with_alpha(color, 0.14 + drive * 0.12 + if active { live_env * 0.1 } else { 0. }));
        kit::line(window, &amp_pts, 2., color);

        // Pitch sweep contour overlay.
        let pitch_pts = kit::sample_curve(r, n, |t| {
            let ms = t * window_ms;
            let p_env = (-ms / sweep_ms.max(2.)).exp();
            let norm = (sweep / 36.0) * p_env;
            (0.88 - norm * 0.78).clamp(0.06, 0.92)
        });
        kit::line(window, &pitch_pts, 1.5, pal.accent2);

        kit::dot(window, r.pt(fx, fy), 6., pal.well, Some(if enabled { pal.text } else { pal.faint }));
        kit::txt(window, cx, &format!("{decay_ms:.0}ms"), r.px(fx) + 8., r.py(fy) - 10., TEXT_XS, color, TextAlign::Left);
    });

    let primary = [p::TUNE, p::SWEEP, p::SWEEP_DECAY, p::DECAY, p::CLICK, p::DRIVE, p::TONE, p::KEY_TRACK];
    let more = [p::GLIDE, p::HOLD, p::VOLUME];

    kit::panel(w)
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .child(caption("808 BODY & PITCH SWEEP"))
                .child(div().flex().gap(px(SP1)).child(kit::readout("ROOT", format!("{tune:.0} Hz"), color)).when(active, |d| {
                    d.child(kit::readout("ENV", format!("{:.0}%", live_env * 100.), pal.accent2))
                })),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(74.))
                .rounded(px(r_sm()))
                .bg(pal.well)
                .border_1()
                .border_color(border())
                .overflow_hidden()
                .child(plot),
        )
        .child(kit::strip(ws, dev, &primary, &more, cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn kick808_editor_reads_fundamental_and_sweep() {
        let d = dev("instrument.kick808", &[(p::TUNE, 55.), (p::SWEEP, 24.)]);
        assert!((d.plain(p::TUNE) - 55.).abs() < 1.0);
        assert!((d.plain(p::SWEEP) - 24.).abs() < 0.1);
    }
}
