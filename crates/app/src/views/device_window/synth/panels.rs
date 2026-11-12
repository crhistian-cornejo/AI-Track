//! Filters (response graph with a draggable cutoff / resonance node), the
//! master strip and the FX tab.

use super::knob::{chip_cell, mknob};
use super::{panel, panel_with};
use crate::theme::*;
use crate::views::device_ui::kit::{self, Handle, PlotCfg, Rect, ax, frac_freq, freq_frac, log_span};
use crate::views::device_ui::wavetable::{FilterView, paint_filter};
use crate::views::device_ui::Dev;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_stock::instrument::wavetable::modulation::ModSlot;
use aitrack_stock::instrument::wavetable::p;
use gpui::{Context, Div, IntoElement, ParentElement, Styled, TextAlign, div, prelude::FluentBuilder, px};
use std::rc::Rc;

const KNOB: f32 = 40.;

/// Filter `n`'s panel.
pub fn filter(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f: u32| p::filt(n, f);
    let f = FilterView::of(dev, n);
    let on = dev.enabled && dev.on(id(p::F_ENABLE));
    let sr = dev.sample_rate;
    let (fx, fy) = f.node(sr);
    let span = dev.info(id(p::F_CUTOFF)).map_or(1., log_span);
    let reset = [id(p::F_CUTOFF), id(p::F_RESO)].map(|x| (x, dev.info(x).map_or(0.5, |i| i.default_normalized()))).to_vec();
    let handles = vec![Handle { fx, fy, x: ax(id(p::F_CUTOFF), span), y: ax(id(p::F_RESO), -1.1), alt: ax(id(p::F_RESO), -1.5), index: 30 + n as usize, reset }];
    let color = if on { [p().accent, p().accent2][n as usize] } else { p().muted };
    // Cutoff modulation: the swept range as a band under the curve.
    let sweep: Vec<(f32, f32, gpui::Hsla)> = super::knob::rings_of(slots, id(p::F_CUTOFF))
        .iter()
        .map(|(_, m)| {
            let (lo, hi) = super::geom::ring_span(dev.norm(id(p::F_CUTOFF)), m.amount, super::geom::slot_is_bipolar(m));
            let info = dev.info(id(p::F_CUTOFF));
            let hz = |v: f32| info.map_or(1000., |i| i.to_plain(v));
            (freq_frac(hz(lo)), freq_frac(hz(hi)), super::source_color(m.source))
        })
        .collect();
    let plot = kit::plot(ws, dev, PlotCfg { slot: 30 + n as u8, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        for (a, b, c) in &sweep {
            window.paint_quad(crate::paint::fill_rect(crate::paint::rect(r.px(*a), r.y, (r.px(*b) - r.px(*a)).max(1.), r.h), with_alpha(*c, 0.1)));
        }
        paint_filter(window, cx, r, f, sr, color, true);
        kit::dot(window, r.pt(fx, fy), 7., color, Some(pal.text));
        let text = format!("{:.0} Hz", frac_freq(fx));
        kit::txt(window, cx, &text, (r.px(fx) + 10.).min(r.right() - 50.), (r.py(fy) - 18.).max(r.y + 4.), TEXT_XS, pal.text, TextAlign::Left);
    });
    let svf = f.model == 0;
    let right = div()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(kit::pick(dev, id(p::F_ENABLE), cx))
        .child(kit::pick(dev, id(p::F_MODEL), cx))
        .when(svf, |d| d.child(kit::pick(dev, id(p::F_TYPE), cx)))
        .when(svf || f.model == 2, |d| d.child(kit::pick(dev, id(p::F_SLOPE), cx)));
    let k = |ws: &mut Workspace, x: u32, cx: &mut Context<Workspace>| mknob(ws, dev, slots, id(x), KNOB, cx);
    let mut el = panel_with(["FILTER 1", "FILTER 2"][n as usize], Some(color), right)
        .w(px(300.))
        .child(div().h(px(132.)).child(plot))
        .child(div().flex().justify_between().child(k(ws, p::F_CUTOFF, cx)).child(k(ws, p::F_RESO, cx)).child(k(ws, p::F_DRIVE, cx)).child(k(ws, p::F_KEY, cx)).child(k(ws, p::F_ENV, cx)));
    if n == 1 {
        el = el.child(div().flex().items_center().gap(px(SP2)).child(label("Filters run", TEXT_XS, p().faint)).child(kit::pick(dev, p::FILTER_ROUTING, cx)).child(label("· each osc picks its filter (Route)", TEXT_XS, p().faint)));
    } else {
        el = el.child(label("Env knob: depth of Env 2 on the cutoff", TEXT_XS, p().faint));
    }
    el
}

/// Voice mode, voices playing and the output volume (top bar).
pub fn master(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let voices = dev.m(0);
    let (l, r) = (dev.m(1), dev.m(2));
    let peak = l.max(r);
    let meter = div().w(px(6.)).h(px(32.)).rounded(px(2.)).bg(pal.well).flex().flex_col().justify_end().child(div().w_full().h(px(32. * peak.clamp(0., 1.))).rounded(px(2.)).bg(if peak > 0.98 { pal.meter_high } else { pal.meter_low }));
    div()
        .flex()
        .items_center()
        .gap(px(SP3))
        .child(div().flex().flex_col().gap(px(SP1)).child(kit::pick(dev, p::VOICE_MODE, cx)).child(kit::readout("voices", format!("{voices:.0}"), pal.text)))
        .child(mknob(ws, dev, slots, p::VOLUME, 32., cx))
        .child(meter)
}

/// A small EQ curve of the synth's 3-band EQ (display only).
fn eq_curve(low: f32, mid: f32, mid_f: f32, high: f32, f: f32) -> f32 {
    let lo = low / (1. + (f / 200.).powi(2));
    let hi = high * (f / 5000.).powi(2) / (1. + (f / 5000.).powi(2));
    let m = mid * (-((f / mid_f.max(20.)).ln() / 0.6).powi(2)).exp();
    lo + hi + m
}

fn fx_block(title: &str, width: f32) -> Div {
    panel(title, Some(p().accent2)).w(px(width))
}

/// The FX tab: distortion → EQ → chorus → delay → reverb, and the voice settings.
pub fn fx(ws: &mut Workspace, dev: &Dev, slots: &Rc<Vec<ModSlot>>, cx: &mut Context<Workspace>) -> Div {
    let k = |ws: &mut Workspace, id: u32, cx: &mut Context<Workspace>| mknob(ws, dev, slots, id, KNOB, cx);
    let (lo, mid, mf, hi) = (dev.plain(p::EQ_LOW), dev.plain(p::EQ_MID), dev.plain(p::EQ_MID_FREQ), dev.plain(p::EQ_HIGH));
    let region = ws.fx.region(dev.id, 50);
    let eq_plot = div().h(px(96.)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(
        gpui::canvas(
            move |b, _, _| region.set(b),
            move |b, _, window, cx| {
                let pal = p();
                let r = Rect::of(b);
                kit::db_grid(window, cx, r, -18., 18., 6., false);
                kit::freq_grid(window, cx, r, false);
                let pts = kit::sample_curve(r, 160, |fx| (18. - eq_curve(lo, mid, mf, hi, frac_freq(fx))) / 36.);
                kit::fill_to(window, &pts, r.py(0.5), with_alpha(pal.accent2, 0.16));
                kit::line(window, &pts, 2., pal.accent2);
            },
        )
        .size_full(),
    );
    let chain = div()
        .flex()
        .gap(px(SP3))
        .child(fx_block("DISTORTION", 220.).child(chip_cell(dev, p::DIST_MODE, 96., cx)).child(div().flex().gap(px(SP2)).child(k(ws, p::DIST_DRIVE, cx)).child(k(ws, p::DIST_MIX, cx))))
        .child(fx_block("EQ", 260.).child(eq_plot).child(div().flex().justify_between().child(k(ws, p::EQ_LOW, cx)).child(k(ws, p::EQ_MID, cx)).child(k(ws, p::EQ_MID_FREQ, cx)).child(k(ws, p::EQ_HIGH, cx))))
        .child(fx_block("CHORUS", 200.).child(div().flex().gap(px(SP2)).child(k(ws, p::CHORUS_MIX, cx)).child(k(ws, p::CHORUS_RATE, cx)).child(k(ws, p::CHORUS_DEPTH, cx))))
        .child(fx_block("DELAY", 200.).child(chip_cell(dev, p::DELAY_TIME, 64., cx)).child(div().flex().gap(px(SP2)).child(k(ws, p::DELAY_MIX, cx)).child(k(ws, p::DELAY_FEEDBACK, cx))))
        .child(fx_block("REVERB", 200.).child(div().flex().gap(px(SP2)).child(k(ws, p::REVERB_MIX, cx)).child(k(ws, p::REVERB_DECAY, cx)).child(k(ws, p::REVERB_TONE, cx))));
    let pal = p();
    let order = div().flex().items_center().gap(px(SP2)).children(["Distortion", "EQ", "Chorus", "Delay", "Reverb", "Volume"].iter().enumerate().map(|(i, s)| {
        div().flex().items_center().gap(px(SP2)).when(i > 0, |d| d.child(mono("→", TEXT_SM, pal.faint))).child(label(*s, TEXT_SM, pal.muted)).into_any_element()
    }));
    let voice = panel("VOICE", Some(pal.accent))
        .child(
            div()
                .flex()
                .items_end()
                .gap(px(SP2))
                .child(chip_cell(dev, p::VOICE_MODE, 72., cx))
                .child(kit::num_w(ws, dev, p::POLYPHONY, 56., cx))
                .child(kit::num_w(ws, dev, p::BEND_RANGE, 56., cx))
                .child(k(ws, p::GLIDE, cx))
                .child(k(ws, p::VEL_SENS, cx))
                .child(k(ws, p::DRIFT, cx))
                .child(k(ws, p::VOLUME, cx)),
        );
    div().flex().flex_col().gap(px(SP3)).child(order).child(chain).child(div().flex().child(voice))
}

#[cfg(test)]
mod tests {
    use super::eq_curve;

    #[test]
    fn the_eq_sketch_follows_its_bands() {
        assert!(eq_curve(0., 0., 1000., 0., 500.).abs() < 1e-6);
        assert!(eq_curve(12., 0., 1000., 0., 30.) > 11.);
        assert!(eq_curve(12., 0., 1000., 0., 10_000.) < 0.5);
        assert!(eq_curve(0., 0., 1000., -12., 18_000.) < -10.);
        assert!((eq_curve(0., 6., 1000., 0., 1000.) - 6.).abs() < 1e-4);
    }
}
