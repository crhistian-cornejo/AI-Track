//! `fx.analyzer`: the plugin's own spectrum, loudness, peak and correlation
//! readings, drawn with the meter strip's painters.

use super::kit::{self, Rect};
use super::Dev;
use crate::theme::*;
use crate::views::meters::{draw_correlation, draw_level_bars, draw_spectrum};
use crate::workspace::Workspace;
use aitrack_stock::fx::analyzer::{BANDS, p};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, canvas, div, px};

fn lin_db(v: f32) -> f32 {
    if v > 1e-6 { 20. * v.log10() } else { -120. }
}

pub fn render(_: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let meter = dev.meter.clone();
    let enabled = dev.enabled;
    let spectrum = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let r = Rect::of(b);
            kit::freq_grid(window, cx, r, true);
            if meter.len() >= BANDS {
                let pal = p();
                draw_spectrum(window, r, &meter[..BANDS], -96., if enabled { pal.accent } else { pal.muted });
            }
        },
    )
    .size_full();
    let m = |i: usize| if dev.meter.len() > i { dev.m(i) } else { -120. };
    let (peaks, rms, corr) = ([lin_db(m(BANDS + 6)), lin_db(m(BANDS + 7))], [lin_db(m(BANDS + 8)), lin_db(m(BANDS + 9))], if dev.meter.len() > BANDS + 10 { dev.m(BANDS + 10) } else { 0. });
    let bars = canvas(
        |_, _, _| {},
        move |b, _, window, cx| {
            let r = Rect::of(b);
            let (levels, phase) = r.split_left(r.w - 14.);
            draw_level_bars(window, cx, levels, peaks, rms);
            draw_correlation(window, phase.inset(2.), corr, true);
        },
    )
    .w(px(64.))
    .h_full()
    .flex_none();
    let lufs = |v: f32| if v > -99. { format!("{v:.1}") } else { "—".to_owned() };
    let readouts = div()
        .flex_none()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(SP1))
        .child(kit::readout("M", lufs(m(BANDS)), pal.text))
        .child(kit::readout("S", lufs(m(BANDS + 1)), pal.text))
        .child(kit::readout("I", lufs(m(BANDS + 2)), pal.accent))
        .child(kit::readout("LRA", format!("{:.1}", m(BANDS + 3).max(0.)), pal.text))
        .child(kit::readout("TP", format!("{} / {}", kit::fmt_db(m(BANDS + 4)), kit::fmt_db(m(BANDS + 5))), pal.text))
        .child(kit::strip(dev, &[p::FFT_SIZE, p::TILT, p::SOURCE, p::FALL, p::RESET], &[], cx));
    kit::panel(if dev.big { 720. } else { 480. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(div().flex_1().min_w(px(0.)).h_full().rounded(px(r_md())).bg(pal.well).border_1().border_color(border()).overflow_hidden().child(spectrum))
                .child(bars),
        )
        .child(readouts)
        .into_any_element()
}
