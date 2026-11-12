//! The "Edit" panels of the Vault window: one per engine, drawn where it is
//! cheap (filter curves with a draggable cutoff / resonance node, ADSR
//! editors, drawbar and register sliders, the FM algorithm diagram), plus
//! the effects and play settings every engine shares.

use super::geom::{fm_layout, is_carrier, targets};
use super::{engine, engine_color};
use crate::theme::*;
use crate::views::device_ui::kit::{self, Handle, PlotCfg, Rect, ax};
use crate::views::device_ui::wavetable::{FilterView, adsr, paint_filter};
use crate::views::device_ui::{Dev, caption};
use crate::widgets::label;
use crate::workspace::Workspace;
use aitrack_stock::instrument::vault::params::{self as vp, engine as eng, ep, fm, ladder, organ, poly, strings, wt};
use aitrack_stock::instrument::wavetable::filter::model;
use aitrack_stock::instrument::wavetable::p as wp;
use gpui::{AnyElement, Context, Div, FontWeight, Hsla, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// Plot slots of the Vault editors (bounds are kept per device and slot).
mod slot {
    pub const FILTER: u8 = 20;
    pub const ENV1: u8 = 21;
    pub const ENV2: u8 = 22;
    pub const SLIDERS: u8 = 23;
    pub const FM_ALGO: u8 = 24;
    pub const AR: u8 = 25;
    /// Operator envelopes: 26..32.
    pub const OP_ENV: u8 = 26;
}

/// A titled card of controls.
fn card(title: impl Into<String>, width: Option<f32>) -> Div {
    let d = div().flex_none().p(px(SP3)).gap(px(SP2)).flex().flex_col().rounded(px(r_lg())).bg(p().surface).border_1().border_color(border()).child(caption(title.into()));
    match width {
        Some(w) => d.w(px(w)),
        None => d,
    }
}

/// A plot area of a fixed height inside a card.
fn area(h: f32) -> Div {
    div().flex_none().w_full().h(px(h)).rounded(px(r_md())).bg(p().well).overflow_hidden()
}

fn default_of(dev: &Dev, id: u32) -> (u32, f32) {
    (id, dev.info(id).map_or(0.5, |i| i.default_normalized()))
}

/// A low-pass response with its node: drag the node for cutoff (sideways)
/// and resonance (up / down, or Alt + drag). `hp` marks a high-pass corner.
#[allow(clippy::too_many_arguments)]
fn filter_plot(ws: &mut Workspace, dev: &Dev, f: FilterView, cutoff: u32, reso: u32, hp: Option<f32>, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let (fx, fy) = f.node(dev.sample_rate);
    let span = dev.info(cutoff).map_or(1., kit::log_span);
    let handles = vec![Handle { fx, fy, x: ax(cutoff, span), y: ax(reso, -1.1), alt: ax(reso, -1.5), index: 0, reset: vec![default_of(dev, cutoff), default_of(dev, reso)] }];
    let sr = dev.sample_rate;
    let on = dev.enabled;
    kit::plot(ws, dev, PlotCfg { slot: slot::FILTER, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let c = if on { color } else { pal.muted };
        paint_filter(window, cx, r, f, sr, c, r.w > 260.);
        if let Some(hz) = hp {
            let x = r.px(kit::freq_frac(hz));
            kit::vline(window, r, x, with_alpha(c, 0.5));
            kit::txt(window, cx, "HP", x + 4., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        }
        kit::dot(window, r.pt(fx, fy), 7., c, Some(pal.text));
    })
    .into_any_element()
}

/// Vertical sliders over `ids` (normalised 0..1 each): drawbars pull down
/// (`down`), registers push up. `colors` per slider, `names` above them.
fn sliders(ws: &mut Workspace, dev: &Dev, ids: &[u32], names: &'static [&'static str], colors: Vec<Hsla>, down: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let n = ids.len().max(1);
    let (top, travel) = (0.16, 0.74);
    let values: Vec<f32> = ids.iter().map(|&id| dev.norm(id)).collect();
    let texts: Vec<String> = ids.iter().map(|&id| dev.text(id)).collect();
    let handles = ids
        .iter()
        .enumerate()
        .map(|(k, &id)| {
            let v = values[k];
            let fy = if down { top + travel * v } else { top + travel * (1. - v) };
            Handle { fx: (k as f32 + 0.5) / n as f32, fy, y: ax(id, if down { 1. / travel } else { -1. / travel }), index: k, reset: vec![default_of(dev, id)], ..Default::default() }
        })
        .collect();
    let on = dev.enabled;
    kit::plot(ws, dev, PlotCfg { slot: slot::SLIDERS, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let w = r.w / n as f32;
        for (k, v) in values.iter().enumerate() {
            let cx0 = r.x + w * (k as f32 + 0.5);
            let c = if on { colors[k] } else { pal.muted };
            let (y0, y1) = (r.py(top), r.py(top + travel));
            // The slot, then the bar: from the top for drawbars, from the bottom for registers.
            kit::bar(window, cx0 - 3., y0, 6., y1 - y0, 3., content(0.1));
            let bw = (w * 0.5).clamp(10., 28.);
            let (a, b) = if down { (y0, y0 + (y1 - y0) * v) } else { (y1 - (y1 - y0) * v, y1) };
            if b - a > 1. {
                kit::bar(window, cx0 - bw / 2., a, bw, b - a, 3., with_alpha(c, 0.85));
            }
            let hy = if down { b } else { a };
            kit::bar(window, cx0 - bw / 2. - 2., hy - 4., bw + 4., 8., 3., c);
            kit::txt(window, cx, names.get(k).copied().unwrap_or(""), cx0, r.y + 4., TEXT_XS, pal.muted, TextAlign::Center);
            kit::txt(window, cx, &texts[k], cx0, r.bottom() - 16., TEXT_XS, pal.text, TextAlign::Center);
        }
    })
    .into_any_element()
}

/// An attack / release envelope (the string machine has no decay stage).
fn ar(ws: &mut Workspace, dev: &Dev, attack: u32, release: u32, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let (a, r) = (dev.norm(attack), dev.norm(release));
    let peak = 0.04 + 0.4 * a;
    let end = (0.6 + 0.04 + 0.36 * r).min(1.);
    let handles = vec![
        Handle { fx: peak, fy: 0.1, x: ax(attack, 1. / 0.4), index: 0, reset: vec![default_of(dev, attack)], ..Default::default() },
        Handle { fx: end, fy: 0.84, x: ax(release, 1. / 0.36), index: 1, reset: vec![default_of(dev, release)], ..Default::default() },
    ];
    let text = format!("Attack {}  Release {}", dev.text(attack), dev.text(release));
    kit::plot(ws, dev, PlotCfg { slot: slot::AR, handles, ..Default::default() }, cx, move |rect, window, cx| {
        let pal = p();
        let pts: Vec<_> = [(0., 0.9), (peak, 0.1), (0.6, 0.1), (end, 0.9)].iter().map(|(x, y)| rect.pt(*x, *y)).collect();
        kit::hline(window, rect, rect.py(0.9), pal.grid_bar);
        kit::fill_to(window, &pts, rect.py(0.9), with_alpha(color, 0.18));
        kit::line(window, &pts, 2., color);
        kit::txt(window, cx, &text, rect.x + 6., rect.y + 3., TEXT_XS, pal.faint, TextAlign::Left);
        kit::dot(window, rect.pt(peak, 0.1), 5.5, color, Some(pal.text));
        kit::dot(window, rect.pt(end, 0.84), 5.5, color, Some(pal.text));
    })
    .into_any_element()
}

/// A low-pass view of a tone control.
fn lowpass(dev: &Dev, cutoff: u32, reso: Option<u32>, slope24: bool) -> FilterView {
    FilterView { model: model::SVF, kind: 0, slope24, cutoff: dev.plain(cutoff), resonance: reso.map_or(0., |r| dev.plain(r) / 100.) }
}

fn row() -> Div {
    div().flex().flex_wrap().items_start().gap(px(SP3))
}

// ───────────────────────────── Engines ─────────────────────────────

fn mono_ladder(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let mut oscs = card("OSCILLATORS", Some(300.));
    for n in 0..3 {
        let id = |f| ladder::osc(n, f);
        oscs = oscs.child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(label(format!("{}", n + 1), TEXT_SM, pal.muted).w(px(12.)))
                .child(kit::pick(dev, id(ladder::WAVE), cx))
                .child(kit::pick(dev, id(ladder::OCTAVE), cx))
                .child(kit::num(ws, dev, id(ladder::DETUNE), cx))
                .child(kit::num(ws, dev, id(ladder::LEVEL), cx)),
        );
    }
    oscs = oscs.child(kit::strip(ws, dev, &[ladder::NOISE, ladder::GLIDE, ladder::LEGATO], &[], cx));
    let f = FilterView { model: model::LADDER, kind: 0, slope24: true, cutoff: dev.plain(ladder::CUTOFF), resonance: dev.plain(ladder::RESONANCE) / 100. };
    let filter = card("LADDER FILTER · 24 dB", Some(340.))
        .child(area(140.).child(filter_plot(ws, dev, f, ladder::CUTOFF, ladder::RESONANCE, None, color, cx)))
        .child(kit::strip(ws, dev, &[ladder::CUTOFF, ladder::RESONANCE, ladder::DRIVE, ladder::ENV_AMOUNT, ladder::KEY_TRACK], &[], cx));
    let envs = card("ENVELOPES", Some(300.))
        .child(label("Filter", TEXT_XS, pal.faint))
        .child(area(72.).child(adsr(ws, dev, slot::ENV1, [ladder::F_ATTACK, ladder::F_DECAY, ladder::F_SUSTAIN, ladder::F_RELEASE], pal.accent2, cx)))
        .child(label("Amp", TEXT_XS, pal.faint))
        .child(area(72.).child(adsr(ws, dev, slot::ENV2, [ladder::A_ATTACK, ladder::A_DECAY, ladder::A_SUSTAIN, ladder::A_RELEASE], color, cx)));
    let modu = card("MODULATION", Some(200.)).child(kit::strip(ws, dev, &[ladder::LFO_RATE, ladder::LFO_PITCH, ladder::LFO_CUTOFF, ladder::VEL_CUTOFF, ladder::VEL_AMP], &[], cx));
    row().child(oscs).child(filter).child(envs).child(modu)
}

fn poly_analog(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let oscs = card("OSCILLATORS", Some(280.))
        .child(label("A", TEXT_XS, pal.faint))
        .child(kit::strip(ws, dev, &[poly::A_WAVE, poly::A_PW, poly::SYNC], &[], cx))
        .child(label("B", TEXT_XS, pal.faint))
        .child(kit::strip(ws, dev, &[poly::B_WAVE, poly::B_SEMI, poly::B_FINE, poly::B_PW], &[], cx))
        .child(kit::strip(ws, dev, &[poly::MIX, poly::NOISE], &[], cx));
    let filter = card("FILTER · 24 dB", Some(340.))
        .child(area(140.).child(filter_plot(ws, dev, lowpass(dev, poly::CUTOFF, Some(poly::RESONANCE), true), poly::CUTOFF, poly::RESONANCE, None, color, cx)))
        .child(kit::strip(ws, dev, &[poly::CUTOFF, poly::RESONANCE, poly::ENV_AMOUNT, poly::KEY_TRACK, poly::VEL_FILTER], &[], cx));
    let envs = card("ENVELOPES", Some(300.))
        .child(label("Filter", TEXT_XS, pal.faint))
        .child(area(72.).child(adsr(ws, dev, slot::ENV1, [poly::F_ATTACK, poly::F_DECAY, poly::F_SUSTAIN, poly::F_RELEASE], pal.accent2, cx)))
        .child(label("Amp", TEXT_XS, pal.faint))
        .child(area(72.).child(adsr(ws, dev, slot::ENV2, [poly::A_ATTACK, poly::A_DECAY, poly::A_SUSTAIN, poly::A_RELEASE], color, cx)));
    let pm = card("POLY-MOD & LFO", Some(280.))
        .child(kit::strip(ws, dev, &[poly::PM_ENV, poly::PM_OSC_B, poly::PM_FREQ, poly::PM_PW, poly::PM_FILTER], &[], cx))
        .child(kit::strip(ws, dev, &[poly::LFO_RATE, poly::LFO_PITCH, poly::LFO_PW, poly::LFO_CUTOFF], &[], cx));
    let voice = card("VOICE", Some(200.)).child(kit::strip(ws, dev, &[poly::DRIFT, poly::CHORUS, poly::GLIDE, poly::VEL_AMP, poly::SPREAD], &[], cx));
    row().child(oscs).child(filter).child(envs).child(pm).child(voice)
}

/// The FM algorithm diagram: operators as boxes (carriers lit, on the
/// bottom row), wires from modulators down to what they modulate, and the
/// feedback loop.
fn fm_diagram(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let index = dev.choice(fm::ALGORITHM);
    let alg = *super::geom::algorithm(index);
    let (cells, cols) = fm_layout(&alg);
    let rows = cells.iter().map(|c| c.1 + 1).max().unwrap_or(1);
    let feedback = dev.plain(fm::FEEDBACK) / 100.;
    let levels: Vec<f32> = (0..6).map(|n| dev.plain(fm::op(n, fm::LEVEL)) / 99.).collect();
    let name = vp::FM_ALGORITHMS.get(index).copied().unwrap_or("");
    let label_text = format!("{}  {name}", index + 1);
    kit::plot(ws, dev, PlotCfg { slot: slot::FM_ALGO, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let r = Rect { y: r.y + 18., h: r.h - 30., ..r };
        let side = 24f32.min(r.w / cols as f32 - 8.).min(r.h / rows as f32 - 8.).max(14.);
        let center = |op: usize| {
            let (c, row) = cells[op];
            let x = r.x + r.w * (c as f32 + 0.5) / cols as f32;
            let y = r.bottom() - r.h * (row as f32 + 0.5) / rows as f32;
            (x, y)
        };
        let wire = with_alpha(pal.text, 0.5);
        for op in 0..6 {
            let (x, y) = center(op);
            for t in targets(&alg, op) {
                let (tx, ty) = center(t);
                kit::line(window, &[gpui::point(px(x), px(y + side / 2.)), gpui::point(px(x), px((y + ty) / 2.)), gpui::point(px(tx), px((y + ty) / 2.)), gpui::point(px(tx), px(ty - side / 2.))], 1.5, wire);
            }
        }
        // Output bus under the carriers.
        let bus = r.bottom() + 6.;
        let carriers: Vec<f32> = (0..6).filter(|&o| is_carrier(&alg, o)).map(|o| center(o).0).collect();
        if let (Some(a), Some(b)) = (carriers.iter().copied().reduce(f32::min), carriers.iter().copied().reduce(f32::max)) {
            for &x in &carriers {
                kit::line(window, &[gpui::point(px(x), px(center(0).1 + side / 2.)), gpui::point(px(x), px(bus))], 1.5, color);
            }
            kit::line(window, &[gpui::point(px(a), px(bus)), gpui::point(px(b), px(bus))], 2., color);
        }
        // Feedback loop on its operator.
        let (fx, fy) = center(alg.feedback);
        let (h, ww) = (side / 2. + 5., side / 2. + 6.);
        kit::line(window, &[gpui::point(px(fx + side / 2.), px(fy)), gpui::point(px(fx + ww), px(fy)), gpui::point(px(fx + ww), px(fy - h)), gpui::point(px(fx), px(fy - h)), gpui::point(px(fx), px(fy - side / 2.))], 1.5, with_alpha(color, 0.3 + 0.7 * feedback));
        for (op, level) in levels.iter().enumerate() {
            let (x, y) = center(op);
            let carrier = is_carrier(&alg, op);
            let fill = if carrier { with_alpha(color, 0.25 + 0.6 * level) } else { mix(pal.raised, color, 0.15 * level) };
            window.paint_quad(crate::paint::rrect_border(crate::paint::rect(x - side / 2., y - side / 2., side, side), r_sm(), fill, if carrier { color } else { border_strong() }, 1.));
            kit::txt(window, cx, &(op + 1).to_string(), x, y - 7., TEXT_SM, pal.text, TextAlign::Center);
        }
        kit::txt(window, cx, &label_text, r.x + 2., r.y - 16., TEXT_XS, pal.faint, TextAlign::Left);
    })
    .into_any_element()
}

fn fm6(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let alg = *super::geom::algorithm(dev.choice(fm::ALGORITHM));
    let algo = card("ALGORITHM", Some(280.))
        .child(area(236.).child(fm_diagram(ws, dev, color, cx)))
        .child(kit::strip(ws, dev, &[fm::ALGORITHM, fm::FEEDBACK, fm::BRIGHTNESS, fm::TIME], &[], cx))
        .child(kit::strip(ws, dev, &[fm::LFO_RATE, fm::LFO_PITCH, fm::LFO_AMP, fm::PITCH_ENV, fm::PITCH_DECAY], &[], cx));
    let mut ops = div().flex().flex_wrap().gap(px(SP2));
    for n in 0..6u32 {
        let id = |f| fm::op(n, f);
        let carrier = is_carrier(&alg, n as usize);
        let c = if carrier { color } else { pal.accent2 };
        let head = div()
            .flex()
            .items_center()
            .gap(px(SP1))
            .child(label(format!("OP {}", n + 1), TEXT_SM, pal.text).font_weight(FontWeight::SEMIBOLD))
            .child(div().flex_1())
            .child(label(if carrier { "carrier" } else { "mod" }, TEXT_XS, c));
        let env = adsr(ws, dev, slot::OP_ENV + n as u8, [id(fm::ATTACK), id(fm::DECAY), id(fm::SUSTAIN), id(fm::RELEASE)], c, cx);
        ops = ops.child(
            div()
                .flex_none()
                .w(px(208.))
                .p(px(SP2))
                .gap(px(SP1))
                .flex()
                .flex_col()
                .rounded(px(r_md()))
                .bg(pal.surface)
                .border_1()
                .border_color(if carrier { with_alpha(color, 0.5) } else { border() })
                .child(head)
                .child(area(48.).child(env))
                .child(kit::strip(ws, dev, &[id(fm::COARSE), id(fm::FINE), id(fm::LEVEL), id(fm::DETUNE), id(fm::VEL), id(fm::KEY_SCALE)], &[], cx)),
        );
    }
    row().child(algo).child(card("OPERATORS", None).w_full().child(ops))
}

fn epiano(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let tine = card("HAMMER & TINE", Some(220.)).child(kit::strip(ws, dev, &[ep::MODEL, ep::HARDNESS, ep::BELL, ep::NOISE, ep::DETUNE], &[], cx));
    let pickup = card("PICKUP & TONE", Some(320.))
        .child(area(110.).child(filter_plot(ws, dev, lowpass(dev, ep::TONE, None, false), ep::TONE, ep::BARK, None, color, cx)))
        .child(kit::strip(ws, dev, &[ep::TONE, ep::BARK, ep::PICKUP], &[], cx));
    let damper = card("DAMPER", Some(160.)).child(kit::strip(ws, dev, &[ep::DECAY, ep::RELEASE, ep::VEL], &[], cx));
    let trem = card("TREMOLO", Some(160.)).child(kit::strip(ws, dev, &[ep::TREM_RATE, ep::TREM_DEPTH, ep::AUTOPAN], &[], cx));
    row().child(tine).child(pickup).child(damper).child(trem)
}

fn drawbar_organ(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let ids: Vec<u32> = (0..9).map(organ::drawbar).collect();
    // Sub-harmonics, the octaves of the 8', and the rest, like the classic bar colours.
    let colors = (0..9).map(|n| match n {
        0 | 1 => pal.accent2,
        2 | 3 | 5 | 8 => pal.text,
        _ => pal.muted,
    }).collect();
    let bars = card("DRAWBARS · pull down to add", Some(420.)).child(area(196.).child(sliders(ws, dev, &ids, vp::DRAWBARS, colors, true, cx)));
    let perc = card("PERCUSSION", Some(180.)).child(kit::strip(ws, dev, &[organ::PERC, organ::PERC_HARM, organ::PERC_DECAY, organ::PERC_VOL, organ::CLICK], &[], cx));
    let rotary = card("ROTARY & TONE", Some(220.))
        .child(kit::strip(ws, dev, &[organ::ROTARY, organ::SPEED, organ::RAMP, organ::VIBRATO], &[], cx))
        .child(kit::strip(ws, dev, &[organ::DRIVE, organ::TONE, organ::RELEASE], &[], cx))
        .child(label(if dev.choice(organ::SPEED) == 1 { "Rotor: fast" } else { "Rotor: slow" }, TEXT_XS, color));
    row().child(bars).child(perc).child(rotary)
}

fn string_machine(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let ids = [strings::CELLO, strings::VIOLIN, strings::VIOLA, strings::HORN];
    let colors = vec![pal.accent2, color, color, pal.solo];
    let regs = card("REGISTERS", Some(220.)).child(area(176.).child(sliders(ws, dev, &ids, &["16'", "8'", "4'", "Horn"], colors, false, cx)));
    let hp = dev.plain(strings::HIGH_PASS);
    let tone = card("TONE", Some(320.))
        .child(area(120.).child(filter_plot(ws, dev, lowpass(dev, strings::TONE, Some(strings::RESONANCE), false), strings::TONE, strings::RESONANCE, Some(hp), color, cx)))
        .child(kit::strip(ws, dev, &[strings::TONE, strings::RESONANCE, strings::HIGH_PASS, strings::SWELL], &[], cx));
    let env = card("ENVELOPE", Some(240.)).child(area(80.).child(ar(ws, dev, strings::ATTACK, strings::RELEASE, color, cx))).child(kit::strip(ws, dev, &[strings::ATTACK, strings::RELEASE], &[], cx));
    let ens = card("ENSEMBLE", Some(180.)).child(kit::strip(ws, dev, &[strings::ENSEMBLE, strings::ENS_DEPTH, strings::ENS_RATE], &[], cx));
    row().child(regs).child(tone).child(env).child(ens)
}

fn wavetable(ws: &mut Workspace, dev: &Dev, color: Hsla, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let oscs = card("OSCILLATORS", Some(280.))
        .child(label("1", TEXT_XS, pal.faint))
        .child(kit::strip(ws, dev, &[wt(wp::osc(0, wp::POS)), wt(wp::osc(0, wp::LEVEL)), wt(wp::osc(0, wp::SEMI)), wt(wp::osc(0, wp::DETUNE))], &[], cx))
        .child(label("2", TEXT_XS, pal.faint))
        .child(kit::strip(ws, dev, &[wt(wp::osc(1, wp::POS)), wt(wp::osc(1, wp::LEVEL)), wt(wp::osc(1, wp::SEMI)), wt(wp::osc(1, wp::DETUNE))], &[], cx))
        .child(kit::strip(ws, dev, &[wt(wp::SUB_LEVEL), wt(wp::NOISE)], &[], cx));
    let f = FilterView { model: model::SVF, kind: dev.choice(wt(wp::FILTER_TYPE)), slope24: dev.choice(wt(wp::FILTER_SLOPE)) == 1, cutoff: dev.plain(wt(wp::CUTOFF)), resonance: dev.plain(wt(wp::RESONANCE)) / 100. };
    let filter = card("FILTER", Some(320.))
        .child(area(120.).child(filter_plot(ws, dev, f, wt(wp::CUTOFF), wt(wp::RESONANCE), None, color, cx)))
        .child(kit::strip(ws, dev, &[wt(wp::CUTOFF), wt(wp::RESONANCE), wt(wp::FILTER_ENV), wt(wp::DRIVE)], &[], cx));
    let env = card("AMP ENVELOPE", Some(260.))
        .child(area(80.).child(adsr(ws, dev, slot::ENV2, [wt(wp::AMP_ATTACK), wt(wp::AMP_DECAY), wt(wp::AMP_SUSTAIN), wt(wp::AMP_RELEASE)], color, cx)))
        .child(label("The wavetables and the mod matrix: add the Wavetable synth itself.", TEXT_XS, pal.faint));
    row().child(oscs).child(filter).child(env)
}

/// The expert panel of the device's engine.
pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let e = engine(dev);
    let color = if dev.enabled { engine_color(e) } else { p().muted };
    let body = match e {
        eng::LADDER => mono_ladder(ws, dev, color, cx),
        eng::POLY => poly_analog(ws, dev, color, cx),
        eng::FM => fm6(ws, dev, color, cx),
        eng::EP => epiano(ws, dev, color, cx),
        eng::ORGAN => drawbar_organ(ws, dev, color, cx),
        eng::STRINGS => string_machine(ws, dev, color, cx),
        _ => wavetable(ws, dev, color, cx),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(SP2))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP2))
                .child(label("EDIT", TEXT_SM, p().faint).font_weight(FontWeight::SEMIBOLD))
                .child(label(super::browse::engine_name(e), TEXT_SM, color).font_weight(FontWeight::SEMIBOLD)),
        )
        .child(body)
        .into_any_element()
}

/// The effects and play settings every engine shares.
pub fn globals(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let depths = [0, 1, 2, 3].map(vp::macro_depth);
    let names: Vec<&str> = aitrack_stock::instrument::vault::macros::macros(engine(dev)).iter().map(|m| m.name).collect();
    let mut depth = div().flex().flex_wrap().gap(px(SP1));
    for (m, id) in depths.iter().enumerate() {
        depth = depth.child(div().flex().flex_col().gap(px(2.)).child(label(names.get(m).copied().unwrap_or(""), TEXT_XS, p().faint)).child(kit::num(ws, dev, *id, cx)));
    }
    row()
        .child(card("EFFECTS", None).child(kit::strip(ws, dev, &[vp::DRIVE, vp::EQ_LOW, vp::EQ_HIGH, vp::DELAY_MIX, vp::DELAY_TIME, vp::DELAY_FEEDBACK, vp::REVERB_MIX, vp::REVERB_DECAY], &[], cx)))
        .child(card("PLAY", None).child(kit::strip(ws, dev, &[vp::ENGINE, vp::VOLUME, vp::BEND_RANGE, vp::POLYPHONY], &[], cx)))
        .child(card("MACRO DEPTH", None).child(depth))
        .into_any_element()
}
