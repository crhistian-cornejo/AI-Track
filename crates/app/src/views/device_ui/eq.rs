//! `fx.eq`: a frequency-response display where every band is a coloured
//! node. Drag = frequency / gain, scroll or Alt + drag = Q (slope on cuts),
//! double-click empty space = new band there, double-click a node = reset,
//! right-click = band menu. Arrow keys nudge the selected band, `[` `]`
//! step through the bands.
//!
//! The curves are computed here with `aitrack_dsp::response`, the same
//! designs the plugin runs (in the Natural / Linear phase modes the
//! analogue prototypes the FIR is designed from), so the picture matches
//! the sound; the spectrum behind them is the plugin's own post-EQ analysis
//! (and, with the curve taken off, what went in). With a sidechain feeding
//! the EQ its spectrum is drawn too, and where the two collide (both loud,
//! close in level) a red band marks the place to carve.

mod collision;

use super::kit::{self, ContextFn, EmptyFn, Handle, PlotCfg, Rect, ax, frac_freq, freq_frac};
use super::{Dev, PopKind, caption, current, set_many, set_now};
use crate::theme::*;
use crate::widgets::{self, label, menu_item, mono};
use crate::workspace::Workspace;
use aitrack_dsp::response::{BandKind, BandResponse};
use aitrack_model::DeviceId;
use aitrack_stock::fx::eq::{BANDS, CURVE_POINTS, SC_POINTS, analog, dynx, meter, p};
use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Point,
    StatefulInteractiveElement, Styled, TextAlign, Window, div, prelude::FluentBuilder, px,
};
use std::rc::Rc;

/// Display-option slot: the dB range of the plot (18 or 30).
const RANGE: u8 = 0;
/// Poles per slope choice (6 dB/oct each).
const SLOPE_ORDER: [usize; 8] = [1, 2, 3, 4, 6, 8, 12, 16];
const TYPE_SHORT: [&str; 8] = ["Bell", "Lo Shelf", "Hi Shelf", "Lo Cut", "Hi Cut", "Notch", "Band", "Tilt"];
const SLOPE_SHORT: [&str; 8] = ["6", "12", "18", "24", "36", "48", "72", "96"];
const CHANNEL_SHORT: [&str; 5] = ["Stereo", "Mid", "Side", "Left", "Right"];
const DIRECTION_SHORT: [&str; 4] = ["Cut Above", "Boost Above", "Cut Below", "Boost Below"];
const SOURCE_SHORT: [&str; 2] = ["Own input", "Sidechain"];

/// One band's settings in plain units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandSpec {
    pub on: bool,
    pub kind: BandKind,
    pub freq: f32,
    pub gain: f32,
    pub q: f32,
    /// Cut slope in poles.
    pub order: usize,
    pub channel: usize,
    pub dynamic: bool,
    pub threshold: f32,
    pub ratio: f32,
    /// Cut Above / Boost Above / Cut Below / Boost Below.
    pub direction: usize,
    pub range: f32,
    /// Detects the external sidechain.
    pub external: bool,
}

/// One band's curve as the plugin runs it in the current phase mode.
pub enum BandCurve {
    Iir(BandResponse, f64),
    Analog(analog::Proto),
}

impl BandCurve {
    pub fn magnitude_at(&self, freq: f64) -> f64 {
        match self {
            BandCurve::Iir(r, offset) => r.magnitude_at(freq) + offset,
            BandCurve::Analog(pr) => {
                let h = pr.at(freq);
                10. * (h.0 * h.0 + h.1 * h.1).max(1e-30).log10()
            }
        }
    }
}

impl BandSpec {
    /// Only bells, shelves and band-passes can be dynamic.
    pub fn can_be_dynamic(&self) -> bool {
        matches!(self.kind, BandKind::Bell | BandKind::LowShelf | BandKind::HighShelf | BandKind::BandPass)
    }

    /// The band as the plugin designs it; `dynamic_db` is the gain change
    /// its dynamics apply right now (signed).
    pub fn response(&self, matched: bool, dynamic_db: f32, sample_rate: f64) -> BandResponse {
        let dynamic = self.dynamic && self.can_be_dynamic();
        let gain = if dynamic { self.gain + dynamic_db } else { self.gain };
        BandResponse::design(self.kind, self.freq, gain, self.q, self.order, matched && !dynamic, sample_rate)
    }

    /// The band's curve in phase mode `phase` (0 minimum, 1 natural, 2 linear).
    pub fn curve(&self, phase: usize, matched: bool, dynamic_db: f32, sample_rate: f64) -> BandCurve {
        let dynamic = self.dynamic && self.can_be_dynamic();
        if phase > 0 {
            let gain = if dynamic && self.kind != BandKind::BandPass { self.gain + dynamic_db } else { self.gain };
            return BandCurve::Analog(analog::Proto::new(self.kind.index(), self.freq as f64, gain as f64, self.q as f64, self.order));
        }
        // A dynamic band-pass turns its whole output up or down.
        let offset = if dynamic && self.kind == BandKind::BandPass { dynamic_db as f64 } else { 0. };
        BandCurve::Iir(self.response(matched, dynamic_db, sample_rate), offset)
    }
}

pub fn band(dev: &Dev, b: usize) -> BandSpec {
    let id = |x: u32| p::band(b, x);
    BandSpec {
        on: dev.on(id(p::ON)),
        kind: BandKind::from_index(dev.choice(id(p::TYPE))),
        freq: dev.plain(id(p::FREQ)),
        gain: dev.plain(id(p::GAIN)),
        q: dev.plain(id(p::Q)),
        order: SLOPE_ORDER[dev.choice(id(p::SLOPE)).min(7)],
        channel: dev.choice(id(p::CHANNEL)),
        dynamic: dev.on(id(p::DYNAMIC)),
        threshold: dev.plain(id(p::THRESHOLD)),
        ratio: dev.plain(id(p::RATIO)),
        direction: dev.choice(dynx::band(b, dynx::MODE)),
        range: dev.plain(dynx::band(b, dynx::RANGE)),
        external: dev.choice(dynx::band(b, dynx::SOURCE)) == 1,
    }
}

/// The summed curve of the enabled bands in dB at `freq` (static gains).
#[cfg(test)]
pub fn curve_db(dev: &Dev, freq: f32) -> f32 {
    let matched = dev.choice(p::MODE) == 0;
    let phase = dev.choice(p::PHASE);
    (0..BANDS).map(|b| band(dev, b)).filter(|s| s.on).map(|s| s.curve(phase, matched, 0., dev.sample_rate).magnitude_at(freq as f64)).sum::<f64>() as f32
}

/// Where a band's node sits in the plot, as fractions of its rect.
pub fn node_pos(kind: BandKind, freq: f32, gain: f32, range: f32) -> (f32, f32) {
    (freq_frac(freq), if kind.uses_gain() { 0.5 - gain / (2. * range) } else { 0.5 })
}

/// The frequency and gain under a point of the plot.
pub fn pos_values(fx: f32, fy: f32, range: f32) -> (f32, f32) {
    (frac_freq(fx), (0.5 - fy) * 2. * range)
}

/// What a new band double-clicked at `(fx, gain)` should be: cuts and
/// shelves at the edges of the spectrum, a bell elsewhere.
pub fn new_band_kind(fx: f32, gain: f32) -> BandKind {
    let flat = gain.abs() < 1.5;
    match fx {
        x if x < 0.12 => if flat { BandKind::LowCut } else { BandKind::LowShelf },
        x if x > 0.88 => if flat { BandKind::HighCut } else { BandKind::HighShelf },
        _ => BandKind::Bell,
    }
}

fn handle(dev: &Dev, b: usize, s: &BandSpec, range: f32) -> Handle {
    let id = |x: u32| p::band(b, x);
    let (fx, fy) = node_pos(s.kind, s.freq, s.gain, range);
    let gain_span = dev.info(id(p::GAIN)).map_or(48., |i| i.max - i.min);
    let default = |x: u32| (id(x), dev.info(id(x)).map_or(0.5, |i| i.default_normalized()));
    Handle {
        fx,
        fy: fy.clamp(0.03, 0.97),
        x: ax(id(p::FREQ), 1.),
        // Bands without gain (notch, band-pass) narrow as the node goes up.
        y: if s.kind.uses_gain() { ax(id(p::GAIN), -2. * range / gain_span) } else if s.kind.uses_q() { ax(id(p::Q), -1.) } else { None },
        alt: if s.kind.uses_slope() { ax(id(p::SLOPE), -1.5) } else if s.kind.uses_q() { ax(id(p::Q), -1.5) } else { None },
        index: b,
        reset: vec![default(p::GAIN), default(p::Q)],
    }
}

fn add_band(ws: &mut Workspace, dev: &Dev, fx: f32, fy: f32, range: f32, cx: &mut Context<Workspace>) {
    let free = (0..BANDS).find(|b| dev.info(p::band(*b, p::ON)).is_some_and(|i| current(ws, dev.id, i) < 0.5));
    let Some(b) = free else {
        ws.toast("All 8 bands are in use", cx);
        return;
    };
    let (freq, gain) = pos_values(fx, fy, range);
    let kind = new_band_kind(fx, gain);
    let id = |x: u32| p::band(b, x);
    let default = |x: u32| dev.info(id(x)).map_or(0.5, |i| i.default_normalized());
    set_many(ws, "Add EQ band", dev.id, &[
        (id(p::ON), 1.),
        (id(p::TYPE), dev.to_norm(id(p::TYPE), kind.index() as f32)),
        (id(p::FREQ), dev.to_norm(id(p::FREQ), freq)),
        (id(p::GAIN), dev.to_norm(id(p::GAIN), if kind.uses_gain() { gain } else { 0. })),
        (id(p::Q), default(p::Q)),
        (id(p::DYNAMIC), 0.),
    ]);
    ws.fx.selected.insert(dev.id, b);
}

struct View {
    bands: Vec<BandSpec>,
    dynamic_db: [f32; BANDS],
    spectrum: Vec<f32>,
    /// The sidechain's spectrum (empty when nothing feeds the EQ).
    sidechain: Vec<f32>,
    selected: usize,
    range: f32,
    matched: bool,
    phase: usize,
    sample_rate: f64,
    enabled: bool,
}

fn paint(v: &View, r: Rect, window: &mut Window, cx: &mut App) {
    let pal = p();
    kit::db_grid(window, cx, r, -v.range, v.range, if v.range > 20. { 10. } else { 6. }, true);
    kit::freq_grid(window, cx, r, true);
    let y_of = |db: f32| r.py((0.5 - db / (2. * v.range)).clamp(0., 1.));
    let zero = y_of(0.);
    let designed: Vec<(usize, BandCurve)> =
        v.bands.iter().enumerate().filter(|(_, s)| s.on).map(|(b, s)| (b, s.curve(v.phase, v.matched, v.dynamic_db[b], v.sample_rate))).collect();

    // The signal: post-EQ filled, pre-EQ (post minus the curve) as an outline.
    if v.spectrum.len() == CURVE_POINTS && v.spectrum.iter().any(|d| *d > -100.) {
        let level = |db: f32| r.py((-db / 96.).clamp(0., 1.));
        let at = |i: usize| i as f32 / (CURVE_POINTS - 1) as f32;
        let post: Vec<Point<Pixels>> = v.spectrum.iter().enumerate().map(|(i, d)| gpui::point(px(r.px(at(i))), px(level(*d)))).collect();
        kit::fill_to(window, &post, r.bottom(), content(0.07));
        kit::line(window, &post, 1., content(0.2));
        if !designed.is_empty() {
            let pre: Vec<Point<Pixels>> = v
                .spectrum
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let f = frac_freq(at(i)) as f64;
                    let eq: f64 = designed.iter().map(|(_, b)| b.magnitude_at(f)).sum();
                    gpui::point(px(r.px(at(i))), px(level(*d - eq as f32)))
                })
                .collect();
            kit::line(window, &pre, 1., with_alpha(pal.accent2, 0.35));
        }
        // The sidechain's spectrum, and where it collides with the track.
        if v.sidechain.len() == SC_POINTS {
            let at = |i: usize| i as f32 / (SC_POINTS - 1) as f32;
            let sc: Vec<Point<Pixels>> = v.sidechain.iter().enumerate().map(|(i, d)| gpui::point(px(r.px(at(i))), px(level(*d)))).collect();
            kit::line(window, &sc, 1., with_alpha(pal.meter_mid, 0.55));
            let hit = collision::collision(&v.spectrum, &v.sidechain);
            let w = r.w / (SC_POINTS - 1) as f32;
            for (i, c) in hit.iter().enumerate().filter(|(_, c)| **c > 0.15) {
                let x = r.px(at(i)) - w / 2.;
                kit::bar(window, x, r.y, w, r.h, 0., with_alpha(pal.record, 0.1 * c));
                kit::bar(window, x, r.bottom() - 4., w, 4., 0., with_alpha(pal.record, 0.35 + 0.5 * c));
            }
        }
    }

    // Each band's own curve, filled in its colour, then the sum on top.
    let n = ((r.w / 3.) as usize).clamp(48, 320);
    let mut total = vec![0.0f32; n + 1];
    for (b, resp) in &designed {
        let color = series(*b);
        let selected = *b == v.selected;
        let pts: Vec<Point<Pixels>> = (0..=n)
            .map(|i| {
                let fx = i as f32 / n as f32;
                let db = resp.magnitude_at(frac_freq(fx) as f64) as f32;
                total[i] += db;
                gpui::point(px(r.px(fx)), px(y_of(db)))
            })
            .collect();
        kit::fill_to(window, &pts, zero, with_alpha(color, if selected { 0.3 } else { 0.14 }));
        kit::line(window, &pts, 1., with_alpha(color, if selected { 0.95 } else { 0.6 }));
    }
    let sum: Vec<Point<Pixels>> = total.iter().enumerate().map(|(i, db)| gpui::point(px(r.px(i as f32 / n as f32)), px(y_of(*db)))).collect();
    kit::line(window, &sum, 2., if v.enabled { pal.text } else { pal.muted });

    // Nodes.
    for (b, s) in v.bands.iter().enumerate().filter(|(_, s)| s.on) {
        let color = series(b);
        let (fx, fy) = node_pos(s.kind, s.freq, s.gain, v.range);
        let c = r.pt(fx, fy.clamp(0.03, 0.97));
        if s.dynamic && s.can_be_dynamic() && v.dynamic_db[b].abs() > 0.1 {
            // How far the dynamics are pulling the band right now.
            let to = y_of(s.gain + v.dynamic_db[b]);
            let (top, h) = (f32::from(c.y).min(to), (f32::from(c.y) - to).abs());
            kit::bar(window, f32::from(c.x) - 1.5, top, 3., h, 1.5, with_alpha(color, 0.8));
        }
        let selected = b == v.selected;
        kit::dot(window, c, if selected { 9. } else { 7. }, color, selected.then_some(pal.text));
        kit::txt(window, cx, &format!("{}", b + 1), f32::from(c.x), f32::from(c.y) - 6.5, TEXT_XS, kit::ink_on(color), TextAlign::Center);
    }
}

/// The exact values of the selected band, floating next to its node.
fn info_panel(ws: &mut Workspace, dev: &Dev, b: usize, s: &BandSpec, range: f32, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    if !s.on {
        return None;
    }
    let pal = p();
    let r = Rect::of(ws.fx.region(dev.id, 0).get());
    if r.w < 240. {
        return None;
    }
    let id = |x: u32| p::band(b, x);
    let color = series(b);
    let (fx, fy) = node_pos(s.kind, s.freq, s.gain, range);
    let width = 204.;
    let dynamic_row = s.can_be_dynamic();
    let dyn_on = dynamic_row && s.dynamic;
    let height = 72. + if dynamic_row { 32. } else { 0. } + if dyn_on { 36. + if dev.big { 36. } else { 0. } } else { 0. };
    let x = r.w * fx;
    let left = if x + 18. + width > r.w { x - 18. - width } else { x + 18. };
    let top = (r.h * fy.clamp(0., 1.) - 28.).clamp(4., (r.h - height - 4.).max(4.));
    let mut values = div()
        .flex()
        .gap(px(SP1))
        .child(kit::num(ws, dev, id(p::FREQ), cx));
    if s.kind.uses_gain() {
        values = values.child(kit::num(ws, dev, id(p::GAIN), cx));
    }
    if s.kind.uses_q() {
        values = values.child(kit::num(ws, dev, id(p::Q), cx));
    }
    if s.kind.uses_slope() {
        values = values.child(div().h(px(32.)).flex().items_center().child(kit::pick(dev, id(p::SLOPE), cx)));
    }
    let mut card = div()
        .absolute()
        .left(px(left.max(4.)))
        .top(px(top))
        .w(px(width))
        .p(px(SP1))
        .gap(px(SP1))
        .flex()
        .flex_col()
        .rounded(px(r_md()))
        .bg(with_alpha(pal.raised, 0.95))
        .border_1()
        .border_color(with_alpha(color, 0.65))
        .shadow_md()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .h(px(20.))
                .gap(px(SP1))
                .flex()
                .items_center()
                .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                .child(label(format!("Band {}", b + 1), TEXT_XS, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(kit::pick(dev, id(p::TYPE), cx))
                .child(kit::pick(dev, id(p::CHANNEL), cx)),
        )
        .child(values);
    if dynamic_row {
        let xid = |x: u32| dynx::band(b, x);
        let mut row = div().h(px(32.)).flex().items_center().gap(px(SP1)).child(kit::pick(dev, id(p::DYNAMIC), cx));
        if s.dynamic {
            row = row.child(kit::pick(dev, xid(dynx::MODE), cx)).child(kit::pick(dev, xid(dynx::SOURCE), cx));
        }
        card = card.child(row);
        if s.dynamic {
            card = card.child(
                div()
                    .flex()
                    .gap(px(SP1))
                    .child(kit::num(ws, dev, id(p::THRESHOLD), cx))
                    .child(kit::num(ws, dev, id(p::RATIO), cx))
                    .child(kit::num(ws, dev, xid(dynx::RANGE), cx)),
            );
            if dev.big {
                card = card.child(
                    div()
                        .flex()
                        .gap(px(SP1))
                        .child(kit::num(ws, dev, id(p::ATTACK), cx))
                        .child(kit::num(ws, dev, id(p::RELEASE), cx))
                        .child(kit::num(ws, dev, xid(dynx::KNEE), cx)),
                );
            }
        }
    }
    Some(card.into_any_element())
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let range = ws.fx.opt(dev.id, RANGE, 18.);
    let bands: Vec<BandSpec> = (0..BANDS).map(|b| band(dev, b)).collect();
    let selected = dev.selected.min(BANDS - 1);
    let device = dev.id;
    let handles = bands.iter().enumerate().filter(|(_, s)| s.on).map(|(b, s)| handle(dev, b, s, range)).collect();
    let adder = dev.clone();
    let on_empty: EmptyFn = Rc::new(move |this, fx, fy, cx| add_band(this, &adder, fx, fy, range, cx));
    let on_context: ContextFn = Rc::new(move |this, band, at, _| this.fx.open_pop(PopKind::EqBand { device, band }, at));
    let mut dynamic_db = [0.0f32; BANDS];
    for (b, d) in dynamic_db.iter_mut().enumerate() {
        *d = dev.m(meter::DYNAMIC + b).clamp(-48., 48.);
    }
    let phase = dev.choice(p::PHASE);
    let sc_active = dev.m(meter::SC_ACTIVE) > 0.5;
    let view = View {
        bands: bands.clone(),
        dynamic_db,
        spectrum: dev.meter.get(meter::SPECTRUM..meter::SPECTRUM + CURVE_POINTS).map(<[f32]>::to_vec).unwrap_or_default(),
        sidechain: if sc_active { dev.meter.get(meter::SC_SPECTRUM..meter::LEN).map(<[f32]>::to_vec).unwrap_or_default() } else { Vec::new() },
        selected,
        range,
        matched: dev.choice(p::MODE) == 0,
        phase,
        sample_rate: dev.sample_rate,
        enabled: dev.enabled,
    };
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, on_empty: Some(on_empty), on_context: Some(on_context), keys: true }, cx, move |r, window, cx| {
        paint(&view, r, window, cx)
    });
    let info = info_panel(ws, dev, selected, &bands[selected], range, cx);

    // One chip per band: the dot switches it on and off, the chip selects it.
    let mut chips = div().flex().items_center().gap(px(SP1));
    for (b, s) in bands.iter().enumerate() {
        let color = series(b);
        let on_id = p::band(b, p::ON);
        let is_on = s.on;
        chips = chips.child(
            div()
                .id(("eq-band", dev.key(b)))
                .flex_none()
                .h(px(24.))
                .px(px(SP1 + 2.))
                .gap(px(SP1))
                .flex()
                .items_center()
                .rounded(px(r_sm()))
                .cursor_pointer()
                .border_1()
                .border_color(if b == selected { with_alpha(color, 0.9) } else { gpui::transparent_black() })
                .bg(if b == selected { with_alpha(color, 0.12) } else { content(0.04) })
                .hover(|st| st.bg(content(0.1)))
                .tooltip(widgets::tip("Select band (dot: on/off, right-click: menu)", None))
                .child(
                    div()
                        .id(("eq-on", dev.key(b)))
                        .size(px(10.))
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(color)
                        .when(is_on, |d| d.bg(color))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            set_now(this, device, on_id, if is_on { 0. } else { 1. });
                            this.fx.selected.insert(device, b);
                            cx.notify();
                        })),
                )
                .child(mono(format!("{}", b + 1), TEXT_XS, if is_on { pal.text } else { pal.faint }))
                .when(dev.big, |d| d.child(label(TYPE_SHORT[s.kind.index()], TEXT_XS, if is_on { pal.muted } else { pal.faint })))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.fx.selected.insert(device, b);
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                        this.fx.selected.insert(device, b);
                        this.fx.open_pop(PopKind::EqBand { device, band: b }, e.position);
                        cx.notify();
                    }),
                ),
        );
    }
    let range_chip = div()
        .id(("eq-range", dev.key(0)))
        .flex_none()
        .h(px(20.))
        .px(px(SP2))
        .flex()
        .items_center()
        .rounded(px(r_sm()))
        .cursor_pointer()
        .bg(content(0.06))
        .hover(|s| s.bg(content(0.12)))
        .tooltip(widgets::tip("Display range: ±18 dB / ±30 dB", None))
        .child(mono(format!("±{range:.0} dB"), TEXT_XS, pal.text))
        .on_click(cx.listener(move |this, _, _, cx| {
            let now = this.fx.opt(device, RANGE, 18.);
            this.fx.opts.insert((device, RANGE), if now > 20. { 18. } else { 30. });
            cx.notify();
        }));

    // Phase mode, with its latency; the minimum-phase flavour only matters there.
    let latency = aitrack_stock::fx::eq::mode_latency(phase, dev.sample_rate);
    let latency_ms = latency as f64 * 1000. / dev.sample_rate.max(1.);
    let phase_group = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SP1))
        .child(kit::pick(dev, p::PHASE, cx))
        .when(dev.big && phase == 0, |d| d.child(kit::pick(dev, p::MODE, cx)))
        .when(latency > 0, |d| d.child(mono(format!("{latency_ms:.1} ms"), TEXT_XS, pal.faint)));
    // The sidechain chip: always in the window, in the dock once it is in use.
    let sidechain_used = dev.choice(p::SIDECHAIN) == 1 || bands.iter().any(|s| s.on && s.dynamic && s.external);
    let sidechain = (dev.big || sidechain_used).then(|| super::sidechain_chip(ws, dev, p::SIDECHAIN, cx));

    kit::panel(if dev.big { 920. } else { 600. })
        .child(div().flex_1().min_h(px(0.)).relative().child(plot).children(info))
        .child(
            div()
                .flex_none()
                .h(px(32.))
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(chips)
                .child(div().flex_1())
                .child(range_chip)
                .child(phase_group)
                .children(sidechain)
                .child(kit::pick(dev, p::AUTO_GAIN, cx))
                .child(kit::num(ws, dev, p::OUTPUT, cx)),
        )
        .into_any_element()
}

/// The right-click menu of a band: type, slope, channel, dynamic, reset, delete.
pub fn band_menu(ws: &mut Workspace, device: DeviceId, band_index: usize, cx: &mut Context<Workspace>) -> Option<AnyElement> {
    let pal = p();
    let d = ws.s.project.device(device)?.clone();
    let bpm = ws.s.project.tempo.initial_bpm();
    let dev = ws.fx.dev(ws.b.engine.as_mut(), ws.b.plugins.as_ref(), &d, aitrack_stock::fx::eq::ID, bpm);
    let s = band(&dev, band_index);
    let id = move |x: u32| p::band(band_index, x);
    let color = series(band_index);
    let grid = |title: &'static str, names: &'static [&'static str], param: u32, now: usize, w: f32, cx: &mut Context<Workspace>| {
        let mut row = div().flex().flex_wrap().gap(px(SP1)).px(px(SP1)).pb(px(SP1));
        for (i, name) in names.iter().enumerate() {
            let norm = dev.to_norm(param, i as f32);
            let on = i == now;
            row = row.child(
                div()
                    .id((title, i))
                    .w(px(w))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .bg(if on { with_alpha(color, 0.24) } else { content(0.05) })
                    .hover(|st| st.bg(content(0.12)))
                    .child(label(*name, TEXT_XS, if on { pal.text } else { pal.muted }).font_weight(FontWeight::MEDIUM))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        set_now(this, device, param, norm);
                        cx.notify();
                    })),
            );
        }
        div().child(caption(title).px(px(SP2)).py(px(SP1))).child(row)
    };
    let mut card = widgets::menu_card("eq-band-menu", 248.)
        .child(
            div()
                .h(px(24.))
                .px(px(SP2))
                .gap(px(SP2))
                .flex()
                .items_center()
                .child(div().size(px(8.)).rounded_full().bg(color))
                .child(label(format!("Band {}", band_index + 1), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
                .child(div().flex_1())
                .child(mono(dev.text(id(p::FREQ)), TEXT_XS, pal.muted)),
        )
        .child(grid("TYPE", &TYPE_SHORT, id(p::TYPE), s.kind.index(), 56., cx));
    if s.kind.uses_slope() {
        card = card.child(grid("SLOPE (dB/oct)", &SLOPE_SHORT, id(p::SLOPE), dev.choice(id(p::SLOPE)), 26., cx));
    }
    card = card.child(grid("CHANNEL", &CHANNEL_SHORT, id(p::CHANNEL), s.channel, 43., cx)).child(widgets::divider_h());
    if s.can_be_dynamic() {
        let next = if s.dynamic { 0. } else { 1. };
        card = card.child(menu_item("eq-dyn", None, "Dynamic", None, s.dynamic).on_click(cx.listener(move |this, _, _, cx| {
            set_now(this, device, id(p::DYNAMIC), next);
            cx.notify();
        })));
        if s.dynamic {
            let xid = move |x: u32| dynx::band(band_index, x);
            card = card
                .child(grid("DYNAMICS", &DIRECTION_SHORT, xid(dynx::MODE), s.direction, 112., cx))
                .child(grid("DETECTOR", &SOURCE_SHORT, xid(dynx::SOURCE), usize::from(s.external), 112., cx));
            if s.external {
                // Pick the track that keys it (and switch the EQ's sidechain on).
                card = card.child(menu_item("eq-sc", None, "Listen to track…", None, false).on_click(cx.listener(move |this, e: &gpui::ClickEvent, _, cx| {
                    let at = e.position();
                    this.fx.open_pop(PopKind::Sidechain { device, param: p::SIDECHAIN }, gpui::point(at.x - px(8.), at.y + px(12.)));
                    cx.notify();
                })));
            }
        }
    }
    let reset = [id(p::GAIN), id(p::Q), id(p::SLOPE)].map(|x| (x, dev.info(x).map_or(0.5, |i| i.default_normalized())));
    let on_next = if s.on { 0. } else { 1. };
    Some(
        card.child(menu_item("eq-reset", None, "Reset gain and Q", None, false).on_click(cx.listener(move |this, _, _, cx| {
            set_many(this, "Reset band", device, &reset);
            this.fx.pop = None;
            cx.notify();
        })))
        .child(menu_item("eq-delete", None, if s.on { "Delete band" } else { "Enable band" }, None, false).on_click(cx.listener(move |this, _, _, cx| {
            set_now(this, device, id(p::ON), on_next);
            this.fx.pop = None;
            cx.notify();
        })))
        .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::super::{Axis, FxDrag};
    use super::*;
    use aitrack_stock::fx::eq::{ID, slope, ty};
    use aitrack_stock::test_util::{process_stereo, set_plain};

    /// Six bands covering every engine the plugin has.
    fn settings() -> Vec<(u32, f32)> {
        let mut v = Vec::new();
        let mut set = |b: usize, t: f32, f: f32, g: f32, q: f32, extra: &[(u32, f32)]| {
            v.extend([(p::band(b, p::ON), 1.), (p::band(b, p::TYPE), t), (p::band(b, p::FREQ), f), (p::band(b, p::GAIN), g), (p::band(b, p::Q), q)]);
            v.extend(extra.iter().map(|(x, val)| (p::band(b, *x), *val)));
        };
        set(0, ty::LOW_CUT, 45., 0., 0.7, &[(p::SLOPE, slope::DB18)]);
        set(1, ty::LOW_SHELF, 120., 4., 0.8, &[]);
        set(2, ty::BELL, 400., -6., 2.5, &[]);
        set(3, ty::NOTCH, 1_800., 0., 6., &[]);
        set(4, ty::BELL, 3_200., 5., 1.1, &[(p::DYNAMIC, 1.), (p::THRESHOLD, 0.)]);
        set(5, ty::TILT, 900., 3., 1., &[]);
        set(6, ty::HIGH_SHELF, 9_000., -3., 0.7, &[]);
        set(7, ty::HIGH_CUT, 17_000., 0., 0.7, &[(p::SLOPE, slope::DB48)]);
        v
    }

    #[test]
    fn the_drawn_curve_is_the_plugins_curve() {
        for mode in [0., 1.] {
            let mut plain = settings();
            plain.push((p::MODE, mode));
            // The plugin, with the same settings, after a block of silence.
            let mut eq = aitrack_stock::create(ID).unwrap();
            for (id, v) in &plain {
                set_plain(eq.as_mut(), *id, *v);
            }
            process_stereo(eq.as_mut(), |_| (0., 0.), 4096);
            let mut meter = [0.0f32; 320];
            assert_eq!(eq.meter(&mut meter), meter::LEN);
            // The editor's maths.
            let d = dev(ID, &plain);
            let mut worst = 0.0f32;
            for (i, plugin_db) in meter[..CURVE_POINTS].iter().enumerate() {
                let f = aitrack_stock::fx::eq::Eq::curve_frequency(i);
                let ui = curve_db(&d, f).max(-120.);
                worst = worst.max((ui - plugin_db).abs());
                assert!((ui - plugin_db).abs() < 0.05, "mode {mode}, {f:.0} Hz: drawn {ui:.3} dB, plugin {plugin_db:.3} dB");
            }
            assert!(worst < 0.05);
            // Spot checks at round frequencies against single-band designs.
            for f in [30., 100., 400., 1_000., 3_200., 10_000., 18_000.] {
                let sum: f64 = (0..BANDS).map(|b| band(&d, b)).filter(|s| s.on).map(|s| s.response(mode == 0., 0., 48_000.).magnitude_at(f as f64)).sum();
                assert!((curve_db(&d, f) - sum as f32).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn the_fir_modes_draw_the_analogue_curve_the_plugin_reports() {
        for phase in [1., 2.] {
            let mut plain = settings();
            plain.push((p::PHASE, phase));
            let mut eq = aitrack_stock::create(ID).unwrap();
            for (id, v) in &plain {
                set_plain(eq.as_mut(), *id, *v);
            }
            process_stereo(eq.as_mut(), |_| (0., 0.), 4096);
            let mut m = [0.0f32; 320];
            assert_eq!(eq.meter(&mut m), meter::LEN);
            assert!(m[meter::LATENCY] > 0.);
            let d = dev(ID, &plain);
            for (i, plugin_db) in m[..CURVE_POINTS].iter().enumerate() {
                let f = aitrack_stock::fx::eq::Eq::curve_frequency(i);
                let ui = curve_db(&d, f).max(-120.);
                assert!((ui - plugin_db).abs() < 0.05, "phase {phase}, {f:.0} Hz: drawn {ui:.3} dB, plugin {plugin_db:.3} dB");
            }
        }
    }

    #[test]
    fn the_band_reads_its_dynamics_settings() {
        let d = dev(ID, &[(p::band(3, p::DYNAMIC), 1.), (dynx::band(3, dynx::MODE), 3.), (dynx::band(3, dynx::RANGE), 7.5), (dynx::band(3, dynx::SOURCE), 1.)]);
        let s = band(&d, 3);
        assert_eq!(s.direction, 3);
        assert!((s.range - 7.5).abs() < 1e-3);
        assert!(s.external);
        assert_eq!(DIRECTION_SHORT.len(), aitrack_stock::fx::eq::DIRECTIONS.len());
        // A boosting band's curve rises with its dynamic gain.
        let bell = BandSpec { on: true, kind: BandKind::Bell, freq: 1000., gain: 0., q: 1., order: 2, channel: 0, dynamic: true, threshold: -20., ratio: 2., direction: 1, range: 6., external: false };
        assert!((bell.curve(0, true, 4., 48_000.).magnitude_at(1000.) - 4.).abs() < 0.05);
        assert!((bell.curve(2, true, 4., 48_000.).magnitude_at(1000.) - 4.).abs() < 0.01);
    }

    #[test]
    fn disabled_bands_leave_the_curve_flat() {
        let d = dev(ID, &[(p::band(2, p::GAIN), 12.)]);
        for f in [50., 1_000., 12_000.] {
            assert_eq!(curve_db(&d, f), 0.);
        }
        assert!(!band(&d, 2).on);
        assert_eq!(band(&d, 0).kind, BandKind::LowCut, "band 1 defaults to a low cut");
    }

    #[test]
    fn nodes_and_values_round_trip() {
        for range in [18., 30.] {
            for (f, g) in [(20., 0.), (63., -12.), (1_000., 6.5), (8_200., 17.9), (20_000., -18.)] {
                let (fx, fy) = node_pos(BandKind::Bell, f, g, range);
                let (f2, g2) = pos_values(fx, fy, range);
                assert!((f2 / f - 1.).abs() < 1e-4 && (g2 - g).abs() < 1e-3, "{f} Hz {g} dB at ±{range}");
            }
            // Zero gain sits on the centre line, +range at the top edge.
            assert_eq!(node_pos(BandKind::Bell, 632.46, 0., range).1, 0.5);
            assert!(node_pos(BandKind::Bell, 100., range, range).1.abs() < 1e-6);
        }
        // Cuts have no gain: they stay on the centre line.
        assert_eq!(node_pos(BandKind::LowCut, 80., 9., 18.).1, 0.5);
    }

    #[test]
    fn dragging_a_node_lands_it_under_the_pointer() {
        let (w, h, range) = (560.0f32, 200.0f32, 18.0f32);
        let d = dev(ID, &[(p::band(2, p::ON), 1.), (p::band(2, p::TYPE), ty::BELL), (p::band(2, p::FREQ), 250.), (p::band(2, p::GAIN), 0.)]);
        let s = band(&d, 2);
        let hd = handle(&d, 2, &s, range);
        assert_eq!(hd.index, 2);
        let axis = |spec: Option<kit::AxisSpec>, extent: f32| spec.map(|a| Axis { param: a.param, value: d.norm(a.param), per_px: a.per_frac / extent });
        let start = (hd.fx * w, hd.fy * h);
        let mut drag = FxDrag { device: d.id, x: axis(hd.x, w), y: axis(hd.y, h), alt: axis(hd.alt, h), last: start };
        // Drag to 3 kHz, +9 dB.
        let (tx, ty_) = node_pos(BandKind::Bell, 3_000., 9., range);
        let changed = drag.advance((tx * w, ty_ * h), false, false);
        let plain = |param: u32| d.info(param).unwrap().to_plain(changed.iter().find(|c| c.0 == param).unwrap().1);
        assert!((plain(p::band(2, p::FREQ)) / 3_000. - 1.).abs() < 2e-3, "freq {}", plain(p::band(2, p::FREQ)));
        assert!((plain(p::band(2, p::GAIN)) - 9.).abs() < 0.02, "gain {}", plain(p::band(2, p::GAIN)));
        // Alt + drag up narrows the band without moving it.
        let q0 = d.plain(p::band(2, p::Q));
        let changed = drag.advance((tx * w, ty_ * h - 30.), false, true);
        assert_eq!(changed.len(), 1);
        assert!(d.info(p::band(2, p::Q)).unwrap().to_plain(changed[0].1) > q0);
        // A cut's node only moves sideways, and Alt changes its slope.
        let cut = handle(&d, 0, &band(&d, 0), range);
        assert!(cut.y.is_none());
        assert_eq!(cut.alt.unwrap().param, p::band(0, p::SLOPE));
    }

    #[test]
    fn new_bands_pick_a_sensible_shape() {
        assert_eq!(new_band_kind(0.5, 6.), BandKind::Bell);
        assert_eq!(new_band_kind(0.05, 0.4), BandKind::LowCut);
        assert_eq!(new_band_kind(0.05, 5.), BandKind::LowShelf);
        assert_eq!(new_band_kind(0.95, -0.5), BandKind::HighCut);
        assert_eq!(new_band_kind(0.95, -8.), BandKind::HighShelf);
    }
}
