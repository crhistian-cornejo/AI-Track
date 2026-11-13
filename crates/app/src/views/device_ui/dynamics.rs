//! `fx.compressor`, `fx.gate` and `fx.transient`: the transfer curve with
//! draggable threshold and ratio / range handles (Alt or scroll on the
//! threshold = knee / hysteresis), a scrolling history of level and gain
//! change, a gain-reduction meter, and the remaining controls in a row.
//!
//! The compressor adds a six-way style selector (Clean, Punch, Glue, Opto,
//! VCA, Vari-Mu: one click writes both the plugin's `Style` and `Model`), the
//! feed-forward / feedback switch and, in the device window, a sidechain
//! column (high-pass, bell, low-pass, listen) next to the history.

use super::kit::{self, Handle, PlotCfg, Rect, ax};
use super::{Dev, HISTORY, caption, set_many};
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::envelope::gain_computer;
use aitrack_stock::fx::{compressor, gate, transient};
use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point, StatefulInteractiveElement, Styled,
    TextAlign, Window, div, prelude::FluentBuilder, px,
};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Compressor,
    Gate,
    Transient,
}

/// The static transfer of a device: output level for an input level, dB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transfer {
    kind: Kind,
    pub threshold: f32,
    pub ratio: f32,
    pub knee: f32,
    /// Compressor: most reduction; gate: gain when closed (≤ 0).
    pub range: f32,
    pub makeup: f32,
    /// Gate mode: 0 gate, 1 expander, 2 duck.
    pub mode: usize,
    pub hysteresis: f32,
    /// Bottom of the plot, dB.
    pub floor: f32,
}

impl Transfer {
    pub fn compressor(dev: &Dev) -> Self {
        use compressor::p;
        // Opto never runs a hard knee, Vari-Mu's ratio grows over 18 dB.
        let knee = compressor::effective_knee(style_now(dev), dev.plain(p::KNEE));
        let (threshold, ratio, range) = (dev.plain(p::THRESHOLD), dev.plain(p::RATIO), dev.plain(p::RANGE));
        let mut makeup = dev.plain(p::MAKEUP) + dev.plain(p::OUTPUT);
        if dev.on(p::AUTO_MAKEUP) {
            makeup += 0.5 * gain_computer(0., threshold, ratio, knee).min(range);
        }
        Self { kind: Kind::Compressor, threshold, ratio, knee, range, makeup, mode: 0, hysteresis: 0., floor: -60. }
    }

    pub fn gate(dev: &Dev) -> Self {
        use gate::p;
        Self {
            kind: Kind::Gate,
            threshold: dev.plain(p::THRESHOLD),
            ratio: dev.plain(p::RATIO),
            knee: 0.,
            range: dev.plain(p::RANGE),
            makeup: 0.,
            mode: dev.choice(p::MODE),
            hysteresis: dev.plain(p::HYSTERESIS),
            floor: -80.,
        }
    }

    /// Output level (dB) for a steady input level (dB).
    pub fn out(&self, level: f32) -> f32 {
        match self.kind {
            Kind::Compressor => level - gain_computer(level, self.threshold, self.ratio, self.knee).min(self.range) + self.makeup,
            Kind::Gate => {
                level
                    + match self.mode {
                        1 => (-(self.threshold - level).max(0.) * (self.ratio - 1.)).max(self.range),
                        2 => if level >= self.threshold { self.range } else { 0. },
                        _ => if level >= self.threshold { 0. } else { self.range },
                    }
            }
            Kind::Transient => level,
        }
    }

    /// Plot fraction of a level on either axis (0 = floor).
    pub fn frac(&self, db: f32) -> f32 {
        ((db - self.floor) / -self.floor).clamp(0., 1.)
    }
}

/// The compressor style in effect (index into `compressor::STYLE_NAMES`).
pub fn style_now(dev: &Dev) -> usize {
    use compressor::p;
    compressor::resolved_style(dev.choice(p::STYLE), dev.choice(p::MODEL))
}

/// The `(Style, Model)` plain values that select style `i` (0 Clean … 5 Vari-Mu).
/// VCA and Vari-Mu keep the classic style underneath as it was.
pub fn style_values(i: usize, classic_now: usize) -> (f32, f32) {
    match i {
        4 => (classic_now as f32, compressor::model::VCA),
        5 => (classic_now as f32, compressor::model::VARI_MU),
        _ => (i.min(3) as f32, compressor::model::CLASSIC),
    }
}

/// The six styles as one segmented control.
fn style_bar(dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    use compressor::p;
    let pal = p();
    let now = style_now(dev);
    let device = dev.id;
    let classic = dev.choice(p::STYLE);
    let mut row = div().flex_none().h(px(20.)).flex().items_center().gap(px(2.)).p(px(2.)).rounded(px(r_sm())).bg(pal.well);
    for (i, name) in compressor::STYLE_NAMES.iter().enumerate() {
        let (st, model) = style_values(i, classic);
        let values = [(p::STYLE, dev.to_norm(p::STYLE, st)), (p::MODEL, dev.to_norm(p::MODEL, model))];
        let on = i == now;
        row = row.child(
            div()
                .id(("comp-style", dev.key(i)))
                .h(px(16.))
                .px(px(SP1 + 2.))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .bg(if on { accent_alpha(0.24) } else { gpui::transparent_black() })
                .hover(|d| d.bg(if on { accent_alpha(0.3) } else { content(0.08) }))
                .child(crate::widgets::label(*name, TEXT_XS, if on { pal.text } else { pal.muted }).font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    set_many(this, "Compressor style", device, &values);
                    cx.notify();
                })),
        );
    }
    row.into_any_element()
}

/// The device window's sidechain column of the compressor.
fn sidechain_column(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    use compressor::p;
    let pal = p();
    let grid = |ws: &mut Workspace, ids: [u32; 2], cx: &mut Context<Workspace>| div().flex().gap(px(SP1)).child(kit::ctl(ws, dev, ids[0], cx)).child(kit::ctl(ws, dev, ids[1], cx));
    div()
        .flex_none()
        .w(px(132.))
        .h_full()
        .p(px(SP2))
        .gap(px(SP1))
        .flex()
        .flex_col()
        .rounded(px(r_md()))
        .bg(pal.well)
        .border_1()
        .border_color(border())
        .child(caption("SIDECHAIN"))
        .child(super::sidechain_chip(ws, dev, p::SIDECHAIN, cx))
        .child(grid(ws, [p::SC_HPF, p::SC_LPF], cx))
        .child(grid(ws, [p::SC_EQ_FREQ, p::SC_EQ_GAIN], cx))
        .child(div().flex().gap(px(SP1)).child(kit::pick(dev, p::SC_LISTEN, cx)).child(kit::pick(dev, p::TOPOLOGY, cx)))
        .into_any_element()
}

/// A history trace as points across `r`, newest at the right edge.
pub fn trace(r: Rect, values: &VecDeque<f32>, fy: impl Fn(f32) -> f32) -> Vec<Point<Pixels>> {
    let skip = HISTORY.saturating_sub(values.len());
    values.iter().enumerate().map(|(i, v)| r.pt((skip + i) as f32 / (HISTORY - 1) as f32, fy(*v).clamp(0., 1.))).collect()
}

fn paint_transfer(t: &Transfer, level: f32, r: Rect, window: &mut Window, cx: &mut App) {
    let pal = p();
    let step = if t.floor < -70. { 20. } else { 12. };
    kit::db_grid(window, cx, r, t.floor, 0., step, false);
    let mut db = t.floor + step;
    while db < -0.1 {
        kit::vline(window, r, r.px(t.frac(db)), pal.grid_beat);
        db += step;
    }
    // Unity, then the curve.
    kit::line(window, &[r.pt(0., 1.), r.pt(1., 0.)], 1., content(0.14));
    let n = 96;
    let pts = kit::sample_curve(r, n, |fx| 1. - t.frac(t.out(t.floor - t.floor * fx)));
    kit::fill_to(window, &pts, r.bottom(), accent_alpha(0.1));
    kit::line(window, &pts, 2., pal.accent);
    let tx = r.px(t.frac(t.threshold));
    kit::vline(window, r, tx, with_alpha(pal.accent2, 0.55));
    if t.kind == Kind::Gate && t.hysteresis > 0.05 && t.mode == 0 {
        kit::vline(window, r, r.px(t.frac(t.threshold - t.hysteresis)), with_alpha(pal.accent2, 0.25));
    }
    kit::txt(window, cx, &format!("{:.1} dB", t.threshold), tx + 4., r.y + 3., TEXT_XS, pal.muted, TextAlign::Left);
    // Where the signal is on the curve right now.
    if level > t.floor + 0.5 {
        kit::dot(window, r.pt(t.frac(level), 1. - t.frac(t.out(level))), 3.5, pal.text, None);
    }
}

/// The prototype of a drum hit and what the shaper does to it.
fn paint_transient(attack: f32, sustain: f32, r: Rect, window: &mut Window, cx: &mut App) {
    let pal = p();
    kit::hline(window, r, r.py(0.92), pal.grid_bar);
    let hit = |fx: f32| if fx < 0.12 { fx / 0.12 } else { (-(fx - 0.12) * 4.2).exp() };
    // Attack scales the first few milliseconds, sustain the tail.
    let shaped = move |fx: f32| {
        let onset = (-((fx - 0.12) / 0.07).powi(2)).exp();
        let tail = ((fx - 0.22) / 0.2).clamp(0., 1.);
        let db = attack / 100. * 12. * onset + sustain / 100. * 12. * tail;
        hit(fx) * 10f32.powf(db / 20.)
    };
    let y = |v: f32| 0.92 - v.clamp(0., 2.) * 0.42;
    let dry = kit::sample_curve(r, 96, |fx| y(hit(fx)));
    kit::line(window, &dry, 1., content(0.3));
    let wet = kit::sample_curve(r, 96, |fx| y(shaped(fx)));
    kit::fill_to(window, &wet, r.py(0.92), accent_alpha(0.14));
    kit::line(window, &wet, 2., pal.accent);
    kit::txt(window, cx, "attack", r.px(0.12), r.y + 3., TEXT_XS, pal.faint, TextAlign::Center);
    kit::txt(window, cx, "sustain", r.px(0.62), r.y + 3., TEXT_XS, pal.faint, TextAlign::Center);
    kit::dot(window, r.pt(0.12, y(shaped(0.12))), 7., series(0), Some(pal.text));
    kit::dot(window, r.pt(0.62, y(shaped(0.62))), 7., series(1), Some(pal.text));
}

pub fn render(ws: &mut Workspace, plugin: &str, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let kind = match plugin {
        "fx.gate" => Kind::Gate,
        "fx.transient" => Kind::Transient,
        _ => Kind::Compressor,
    };
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let span = |id: u32| dev.info(id).map_or(1., |i| i.max - i.min);
    let (transfer, handles, primary, more): (Option<Transfer>, Vec<Handle>, Vec<u32>, Vec<u32>) = match kind {
        Kind::Compressor => {
            use compressor::p;
            let t = Transfer::compressor(dev);
            let edge = -1.5;
            let handles = vec![
                Handle {
                    fx: t.frac(t.threshold),
                    fy: 1. - t.frac(t.out(t.threshold)),
                    x: ax(p::THRESHOLD, -t.floor / span(p::THRESHOLD)),
                    alt: ax(p::KNEE, -1.5),
                    index: 0,
                    reset: vec![default(p::THRESHOLD), default(p::KNEE)],
                    ..Default::default()
                },
                Handle { fx: t.frac(edge), fy: 1. - t.frac(t.out(edge)), y: ax(p::RATIO, 1.2), index: 1, reset: vec![default(p::RATIO)], ..Default::default() },
            ];
            (
                Some(t),
                handles,
                if dev.big { vec![p::ATTACK, p::RELEASE, p::KNEE, p::MAKEUP, p::MIX] } else { vec![p::ATTACK, p::RELEASE, p::KNEE, p::MAKEUP, p::MIX, p::TOPOLOGY] },
                // In the window the sidechain controls live in their own column.
                vec![p::THRESHOLD, p::RATIO, p::RANGE, p::RMS, p::LINK, p::LOOKAHEAD, p::OUTPUT, p::AUTO_RELEASE, p::AUTO_MAKEUP],
            )
        }
        Kind::Gate => {
            use gate::p;
            let t = Transfer::gate(dev);
            let low = t.floor * 0.82;
            let handles = vec![
                Handle {
                    fx: t.frac(t.threshold),
                    fy: 1. - t.frac(t.threshold),
                    x: ax(p::THRESHOLD, -t.floor / span(p::THRESHOLD)),
                    alt: ax(p::HYSTERESIS, -1.5),
                    index: 0,
                    reset: vec![default(p::THRESHOLD), default(p::HYSTERESIS)],
                    ..Default::default()
                },
                Handle {
                    fx: t.frac(low),
                    fy: (1. - t.frac(low + t.range)).clamp(0.04, 0.96),
                    y: ax(p::RANGE, t.floor / span(p::RANGE)),
                    index: 1,
                    reset: vec![default(p::RANGE)],
                    ..Default::default()
                },
            ];
            (
                Some(t),
                handles,
                vec![p::ATTACK, p::HOLD, p::RELEASE, p::HYSTERESIS, p::MODE],
                vec![p::THRESHOLD, p::RANGE, p::RATIO, p::LOOKAHEAD, p::SC_HPF, p::SC_LPF, p::SIDECHAIN],
            )
        }
        Kind::Transient => {
            use transient::p;
            let handles = vec![
                Handle { fx: 0.12, fy: 0.4, y: ax(p::ATTACK, -1.4), index: 0, reset: vec![default(p::ATTACK)], ..Default::default() },
                Handle { fx: 0.62, fy: 0.75, y: ax(p::SUSTAIN, -1.4), index: 1, reset: vec![default(p::SUSTAIN)], ..Default::default() },
            ];
            (None, handles, vec![p::ATTACK, p::SUSTAIN, p::WINDOW, p::LENGTH, p::MIX], vec![p::OUTPUT, p::CLIP])
        }
    };

    // Left: the curve. Meter slot 1 is the detector / input level.
    let level = if dev.meter.len() > 1 { dev.m(1) } else { -120. };
    let (attack, sustain) = (dev.plain(transient::p::ATTACK), dev.plain(transient::p::SUSTAIN));
    let curve = kit::plot(ws, dev, PlotCfg { slot: 0, handles: handles.clone(), keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        match transfer {
            Some(t) => {
                paint_transfer(&t, level, r, window, cx);
                for (i, h) in handles.iter().enumerate() {
                    kit::dot(window, r.pt(h.fx, h.fy), 7., series(i), Some(pal.text));
                }
            }
            None => paint_transient(attack, sustain, r, window, cx),
        }
    });

    // Middle: level and gain change over the last four seconds.
    let hist = dev.hist.clone();
    let region = ws.fx.region(dev.id, 1);
    let history = div().flex_1().min_w(px(0.)).min_h(px(0.)).rounded(px(r_md())).bg(pal.well).border_1().border_color(border()).overflow_hidden().child(
        gpui::canvas(
            move |b, _, _| region.set(b),
            move |b, _, window, cx| {
                let pal = p();
                let r = Rect::of(b);
                let h = hist.borrow();
                match kind {
                    Kind::Transient => {
                        // Gain applied, around the centre line (±12 dB).
                        kit::hline(window, r, r.py(0.5), pal.grid_bar);
                        let pts = trace(r, &h.traces[0], |db| 0.5 - db / 24.);
                        kit::fill_to(window, &pts, r.py(0.5), accent_alpha(0.25));
                        kit::line(window, &pts, 1.5, pal.accent);
                        kit::txt(window, cx, "gain ±12 dB", r.x + 4., r.y + 3., TEXT_XS, pal.faint, TextAlign::Left);
                    }
                    _ => {
                        let t = transfer.unwrap_or(Transfer { kind, threshold: 0., ratio: 1., knee: 0., range: 0., makeup: 0., mode: 0, hysteresis: 0., floor: -60. });
                        let lv = |db: f32| 1. - t.frac(db);
                        kit::db_grid(window, cx, r, t.floor, 0., if t.floor < -70. { 20. } else { 12. }, true);
                        // Input (or detector) level from the bottom.
                        let input = trace(r, &h.traces[1], lv);
                        kit::fill_to(window, &input, r.bottom(), content(0.1));
                        kit::line(window, &input, 1., content(0.3));
                        if kind == Kind::Compressor {
                            kit::line(window, &trace(r, &h.traces[2], lv), 1.5, pal.accent2);
                        }
                        kit::hline(window, r, r.py(lv(t.threshold)), with_alpha(pal.accent2, 0.5));
                        // Gain change hangs from the top (24 dB over half the height).
                        let gr = trace(r, &h.traces[0], |db| (-db / 48.).min(0.5));
                        kit::fill_to(window, &gr, r.y, with_alpha(pal.record, 0.35));
                        kit::line(window, &gr, 1.5, pal.record);
                    }
                }
            },
        )
        .size_full(),
    );

    // Right: the gain change right now.
    let now = dev.m(0);
    let meter = div().flex_none().w(px(40.)).h_full().flex().flex_col().items_center().gap(px(SP1)).child(
        div().flex_1().w(px(12.)).rounded(px(3.)).bg(pal.well).border_1().border_color(border()).overflow_hidden().child(
            gpui::canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    let pal = p();
                    let r = Rect::of(b);
                    match kind {
                        Kind::Transient => {
                            let h = (now / 12.).clamp(-1., 1.) * r.h / 2.;
                            kit::bar(window, r.x, if h > 0. { r.y + r.h / 2. - h } else { r.y + r.h / 2. }, r.w, h.abs(), 0., pal.accent);
                        }
                        _ => kit::bar(window, r.x, r.y, r.w, (-now / 24.).clamp(0., 1.) * r.h, 0., pal.record),
                    }
                },
            )
            .size_full(),
        ),
    )
    .child(crate::widgets::mono(format!("{now:+.1}"), TEXT_XS, if now.abs() > 0.05 { pal.text } else { pal.faint }))
    .child(crate::widgets::label(if kind == Kind::Transient { "gain" } else { "GR" }, TEXT_XS, pal.faint));

    // Compressor and gate can listen to another track.
    let sidechain = match plugin {
        "fx.compressor" => Some(super::sidechain_chip(ws, dev, aitrack_stock::fx::compressor::p::SIDECHAIN, cx)),
        "fx.gate" => Some(super::sidechain_chip(ws, dev, aitrack_stock::fx::gate::p::SIDECHAIN, cx)),
        _ => None,
    };
    // The compressor's middle column: style above the history, with what the
    // detector is doing (feedback, lookahead) as a badge.
    let middle = if kind == Kind::Compressor {
        use compressor::p;
        let fb = dev.m(69) > 0.5 || (dev.meter.len() < 70 && dev.choice(p::TOPOLOGY) == 1 && dev.choice(p::SIDECHAIN) == 0);
        let look = dev.plain(p::LOOKAHEAD);
        let badge = match (fb, look > 0.05) {
            (true, _) => Some("feedback".to_owned()),
            (false, true) => Some(format!("lookahead {look:.1} ms")),
            _ => None,
        };
        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(SP1))
            .child(div().flex_none().flex().items_center().gap(px(SP2)).child(style_bar(dev, cx)).child(div().flex_1()).when_some(badge, |d, b| d.child(crate::widgets::mono(b, TEXT_XS, pal.faint))))
            .child(history)
            .into_any_element()
    } else {
        history.h_full().into_any_element()
    };
    let side = (kind == Kind::Compressor && dev.big).then(|| sidechain_column(ws, dev, cx));
    let sidechain = if side.is_some() { None } else { sidechain };
    kit::panel(if dev.big { 860. } else { 540. })
        .child(
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .gap(px(SP2))
                .child(div().flex_none().w(px(if dev.big { 240. } else { 176. })).h_full().child(curve))
                .child(middle)
                .child(meter)
                .children(side),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(kit::strip(ws, dev, &primary, &more, cx))
                .children(sidechain),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;
    use aitrack_stock::test_util::set_plain;

    #[test]
    fn the_compressor_curve_is_the_plugins_static_curve() {
        use compressor::p;
        let plain = [(p::THRESHOLD, -24.), (p::RATIO, 6.), (p::KNEE, 9.), (p::MAKEUP, 3.), (p::AUTO_MAKEUP, 1.), (p::RANGE, 18.)];
        let mut plugin = aitrack_stock::create("fx.compressor").unwrap();
        for (id, v) in plain {
            set_plain(plugin.as_mut(), id, v);
        }
        aitrack_stock::test_util::process_stereo(plugin.as_mut(), |_| (0., 0.), 2048);
        let mut m = [0.0f32; 80];
        assert_eq!(plugin.meter(&mut m), 70);
        let t = Transfer::compressor(&dev("fx.compressor", &plain));
        for (i, plugin_db) in m[3..67].iter().enumerate() {
            let level = -60. + 60. * i as f32 / 63.;
            assert!((t.out(level) - plugin_db).abs() < 0.05, "{level:.1} dB in: drawn {:.2}, plugin {plugin_db:.2}", t.out(level));
        }
        // Above the knee the slope is 1/ratio; the range caps the reduction.
        assert!((t.out(-6.) - t.out(-12.) - 1.).abs() < 0.01);
        let capped = Transfer { range: 4., ..t };
        assert!((capped.out(0.) - (0. - 4. + capped.makeup)).abs() < 1e-4);
    }

    #[test]
    fn the_style_selector_writes_style_and_model() {
        use compressor::p;
        assert_eq!(style_values(2, 0), (2., compressor::model::CLASSIC));
        assert_eq!(style_values(4, 3), (3., compressor::model::VCA));
        assert_eq!(style_values(5, 1), (1., compressor::model::VARI_MU));
        for i in 0..6 {
            let (st, model) = style_values(i, 2);
            assert_eq!(style_now(&dev("fx.compressor", &[(p::STYLE, st), (p::MODEL, model)])), i);
        }
        // The Vari-Mu knee is the plugin's.
        let plain = [(p::MODEL, compressor::model::VARI_MU), (p::KNEE, 3.), (p::THRESHOLD, -20.)];
        let mut plugin = aitrack_stock::create("fx.compressor").unwrap();
        for (id, v) in plain {
            set_plain(plugin.as_mut(), id, v);
        }
        aitrack_stock::test_util::process_stereo(plugin.as_mut(), |_| (0., 0.), 1024);
        let mut m = [0.0f32; 80];
        plugin.meter(&mut m);
        let t = Transfer::compressor(&dev("fx.compressor", &plain));
        assert_eq!(t.knee, 18.);
        for (i, plugin_db) in m[3..67].iter().enumerate() {
            let level = -60. + 60. * i as f32 / 63.;
            assert!((t.out(level) - plugin_db).abs() < 0.05);
        }
    }

    #[test]
    fn gate_modes_have_their_own_curves() {
        use gate::p;
        let gate = Transfer::gate(&dev("fx.gate", &[(p::THRESHOLD, -40.), (p::RANGE, -60.)]));
        assert_eq!(gate.out(-30.), -30.);
        assert_eq!(gate.out(-50.), -110.);
        let exp = Transfer::gate(&dev("fx.gate", &[(p::THRESHOLD, -40.), (p::RANGE, -60.), (p::MODE, 1.), (p::RATIO, 3.)]));
        assert_eq!(exp.out(-45.), -55.);
        assert_eq!(exp.out(-20.), -20.);
        let duck = Transfer::gate(&dev("fx.gate", &[(p::THRESHOLD, -40.), (p::RANGE, -12.), (p::MODE, 2.)]));
        assert_eq!(duck.out(-30.), -42.);
        assert_eq!(duck.out(-50.), -50.);
        assert_eq!(gate.frac(-80.), 0.);
        assert_eq!(gate.frac(0.), 1.);
    }

    #[test]
    fn history_traces_end_at_the_right_edge() {
        let r = Rect { x: 0., y: 0., w: 239., h: 100. };
        let values: VecDeque<f32> = (0..10).map(|i| i as f32 / 10.).collect();
        let pts = trace(r, &values, |v| v);
        assert_eq!(pts.len(), 10);
        assert!((f32::from(pts[9].x) - 239.).abs() < 1e-3);
        assert!((f32::from(pts[0].x) - 230.).abs() < 1e-3);
    }
}
