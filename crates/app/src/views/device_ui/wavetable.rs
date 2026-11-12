//! `instrument.wavetable`: the oscillators' wavetables at their position
//! (drag across the display = position), the filter's response with a
//! draggable cutoff / resonance node, ADSR envelopes with draggable points,
//! the LFO shapes, and the modulation matrix as a table. The compact card
//! shows oscillators, filter and amp envelope; the expanded one everything.

use super::chorus::lfo_value;
use super::filter::{node, paint_response};
use super::kit::{self, Handle, PlotCfg, ax};
use super::{Dev, caption};
use crate::theme::*;
use crate::widgets::{label, mono};
use crate::workspace::Workspace;
use aitrack_dsp::lfo::LfoShape;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode};
use aitrack_dsp::wavetable::Wavetable;
use aitrack_stock::instrument::wavetable::p;
use gpui::{AnyElement, Context, Div, Hsla, IntoElement, ParentElement, Styled, TextAlign, div, px};
use std::rc::Rc;

/// The sections the synth's voice filter runs (see `WavetableSynth`).
pub fn filter_sections(mode: usize, slope24: bool, cutoff: f32, resonance: f32, sample_rate: f64) -> Vec<SvfCoeffs> {
    let mode = match mode {
        1 => SvfMode::Highpass,
        2 => SvfMode::Bandpass,
        _ => SvfMode::Lowpass,
    };
    let r = resonance.clamp(0., 1.);
    if slope24 {
        vec![
            SvfCoeffs::new(mode, cutoff, 0.541 * (1. + r * 0.2), 0., sample_rate),
            SvfCoeffs::new(mode, cutoff, 1.307 * (20f32 / 1.307).powf(r), 0., sample_rate),
        ]
    } else {
        let q = std::f32::consts::FRAC_1_SQRT_2 * (20f32 / std::f32::consts::FRAC_1_SQRT_2).powf(r);
        vec![SvfCoeffs::new(mode, cutoff, q, 0., sample_rate)]
    }
}

/// The corner points of an ADSR drawn in a unit square: start, attack peak,
/// decay end, release start, end. Times are the parameters' normalised
/// values, so the log scale of the knobs is the scale of the picture.
pub fn adsr_points(attack: f32, decay: f32, sustain: f32, release: f32) -> [(f32, f32); 5] {
    let (top, bottom) = (0.1, 0.9);
    let a = 0.02 + 0.24 * attack;
    let d = a + 0.02 + 0.24 * decay;
    let s = d + 0.18;
    let level = bottom - (bottom - top) * sustain.clamp(0., 1.);
    [(0., bottom), (a, top), (d, level), (s, level), ((s + 0.02 + 0.28 * release).min(1.), bottom)]
}

/// An envelope editor: drag the peak (attack), the knee (decay / sustain)
/// and the end (release). `ids` are attack, decay, sustain, release.
pub fn adsr(ws: &mut Workspace, dev: &Dev, slot: u8, ids: [u32; 4], color: Hsla, cx: &mut Context<Workspace>) -> AnyElement {
    let [a, d, s, r] = ids;
    let pts = adsr_points(dev.norm(a), dev.norm(d), dev.plain(s) / 100., dev.norm(r));
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let base = slot as usize * 8;
    let handles = vec![
        Handle { fx: pts[1].0, fy: pts[1].1, x: ax(a, 1. / 0.24), index: base, reset: vec![default(a)], ..Default::default() },
        Handle { fx: pts[2].0, fy: pts[2].1, x: ax(d, 1. / 0.24), y: ax(s, -1. / 0.8), index: base + 1, reset: vec![default(d), default(s)], ..Default::default() },
        Handle { fx: pts[4].0, fy: pts[4].1 - 0.06, x: ax(r, 1. / 0.28), index: base + 2, reset: vec![default(r)], ..Default::default() },
    ];
    let marks = handles.clone();
    let text = format!("A {}  D {}  S {}  R {}", dev.text(a), dev.text(d), dev.text(s), dev.text(r));
    kit::plot(ws, dev, PlotCfg { slot, handles, ..Default::default() }, cx, move |rect, window, cx| {
        let pal = p();
        kit::hline(window, rect, rect.py(0.9), pal.grid_bar);
        let line: Vec<_> = pts.iter().map(|(x, y)| rect.pt(*x, *y)).collect();
        kit::fill_to(window, &line, rect.py(0.9), with_alpha(color, 0.18));
        kit::line(window, &line, 2., color);
        if rect.w > 200. {
            kit::txt(window, cx, &text, rect.x + 6., rect.y + 3., TEXT_XS, pal.faint, TextAlign::Left);
        }
        for h in &marks {
            kit::dot(window, rect.pt(h.fx, h.fy), 5.5, color, Some(pal.text));
        }
    })
    .into_any_element()
}

fn tables(ws: &mut Workspace) -> Rc<Vec<Wavetable>> {
    ws.fx
        .tables
        .get_or_insert_with(|| {
            // The synth's bank, in `TABLES` order.
            Rc::new(vec![Wavetable::saw(), Wavetable::square(), Wavetable::pulse_morph(16), Wavetable::sine_to_saw(16), Wavetable::triangle(), Wavetable::sine()])
        })
        .clone()
}

fn section(title: &str, width: f32) -> Div {
    div().flex_none().w(px(width)).h_full().flex().flex_col().gap(px(SP1)).child(caption(title.to_owned()))
}

fn osc(ws: &mut Workspace, dev: &Dev, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f: u32| p::osc(n, f);
    let bank = tables(ws);
    let table = dev.choice(id(p::TABLE)).min(bank.len() - 1);
    let (pos, level) = (dev.plain(id(p::POS)) / 100., dev.plain(id(p::LEVEL)) / 100.);
    let default = |x: u32| (x, dev.info(x).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![Handle { fx: pos.clamp(0.02, 0.98), fy: 0.9, x: ax(id(p::POS), 1.), index: n as usize, reset: vec![default(id(p::POS))], ..Default::default() }];
    let color = if dev.enabled { series(n as usize) } else { p().muted };
    let display = kit::plot(ws, dev, PlotCfg { slot: n as u8, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.45), pal.grid_bar);
        let t = &bank[table];
        let wave = |at: f32| kit::sample_curve(r, 128, |fx| 0.45 - 0.36 * t.sample(0, at, fx.min(0.9999)));
        // Neighbouring frames of the table behind the one that plays.
        for ghost in [pos - 0.33, pos + 0.33] {
            if (0. ..=1.).contains(&ghost) {
                kit::line(window, &wave(ghost), 1., with_alpha(color, 0.25));
            }
        }
        let now = wave(pos);
        kit::fill_to(window, &now, r.py(0.45), with_alpha(color, 0.12 + 0.2 * level));
        kit::line(window, &now, 2., color);
        // Position along the table.
        kit::bar(window, r.x + 6., r.py(0.9) - 1., r.w - 12., 2., 1., content(0.14));
        kit::dot(window, r.pt(pos.clamp(0.02, 0.98), 0.9), 5.5, color, Some(pal.text));
        kit::txt(window, cx, &format!("pos {:.0}%", pos * 100.), r.right() - 6., r.y + 3., TEXT_XS, pal.faint, TextAlign::Right);
    });
    section(if n == 0 { "OSC 1" } else { "OSC 2" }, 196.)
        .child(div().flex_1().min_h(px(40.)).child(display))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, id(p::TABLE), cx))
                .child(kit::strip(ws, dev, &[id(p::LEVEL)], &[], cx)),
        )
        .child(kit::strip(
            ws,
            dev,
            &[id(p::OCTAVE), id(p::SEMI), id(p::FINE)],
            &[id(p::UNISON), id(p::DETUNE), id(p::SPREAD), id(p::PAN)],
            cx,
        ))
}

fn filter(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let (cutoff, reso) = (dev.plain(p::CUTOFF), dev.plain(p::RESONANCE) / 100.);
    let secs = filter_sections(dev.choice(p::FILTER_TYPE), dev.choice(p::FILTER_SLOPE) == 1, cutoff, reso, dev.sample_rate);
    let (fx, fy) = node(&secs, cutoff, dev.sample_rate);
    let reset = [p::CUTOFF, p::RESONANCE].map(|id| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()))).to_vec();
    let handles = vec![Handle { fx, fy, x: ax(p::CUTOFF, 1.), y: ax(p::RESONANCE, -1.1), alt: ax(p::RESONANCE, -1.5), index: 2, reset }];
    let (sr, enabled) = (dev.sample_rate, dev.enabled);
    let plot = kit::plot(ws, dev, PlotCfg { slot: 2, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        paint_response(window, cx, r, &secs, sr, if enabled { pal.accent } else { pal.muted }, r.w > 260.);
        kit::dot(window, r.pt(fx, fy), 7., pal.accent, Some(pal.text));
    });
    section("FILTER", 236.)
        .child(div().flex_1().min_h(px(40.)).child(plot))
        .child(div().flex().items_center().gap(px(SP1)).child(kit::pick(dev, p::FILTER_TYPE, cx)).child(kit::pick(dev, p::FILTER_SLOPE, cx)))
        .child(kit::strip(
            ws,
            dev,
            &[p::DRIVE, p::FILTER_ENV, p::KEY_TRACK],
            &[p::CUTOFF, p::RESONANCE],
            cx,
        ))
}

fn lfo(ws: &mut Workspace, dev: &Dev, n: u32, cx: &mut Context<Workspace>) -> Div {
    let id = |f: u32| p::lfo(n, f);
    let shape = LfoShape::from_index(dev.choice(id(p::LFO_SHAPE)));
    let color = series(4 + n as usize);
    let region = ws.fx.region(dev.id, 5 + n as u8);
    let display = div().flex_1().min_h(px(32.)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(
        gpui::canvas(
            move |b, _, _| region.set(b),
            move |b, _, window, _| {
                let pal = p();
                let r = kit::Rect::of(b);
                kit::hline(window, r, r.py(0.5), pal.grid_bar);
                kit::vline(window, r, r.px(0.5), pal.grid_beat);
                // Two cycles.
                let pts = kit::sample_curve(r, 160, |fx| 0.5 - 0.38 * lfo_value(shape, fx * 2.));
                kit::fill_to(window, &pts, r.py(0.5), with_alpha(color, 0.14));
                kit::line(window, &pts, 1.5, color);
            },
        )
        .size_full(),
    );
    let rate = if dev.on(id(p::LFO_SYNC)) { id(p::LFO_DIVISION) } else { id(p::LFO_RATE) };
    section(if n == 0 { "LFO 1" } else { "LFO 2" }, 172.)
        .child(display)
        .child(div().flex().flex_wrap().items_center().gap(px(SP1)).child(kit::pick(dev, id(p::LFO_SHAPE), cx)).child(kit::pick(dev, id(p::LFO_SYNC), cx)).child(kit::pick(dev, id(p::LFO_RETRIGGER), cx)))
        .child(kit::strip(ws, dev, &[rate], &[], cx))
}

/// The modulation matrix: one row per slot, source → destination, amount.
fn matrix(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let mut table = section("MODULATION", 312.);
    let head = |text: &'static str, w: f32| label(text, TEXT_XS, pal.faint).w(px(w));
    table = table.child(div().flex().gap(px(SP1)).pl(px(18.)).child(head("Source", 96.)).child(head("Destination", 108.)).child(head("Amount", 60.)));
    for s in 0..8u32 {
        let active = dev.choice(p::slot(s, p::MOD_SRC)) != 0 && dev.choice(p::slot(s, p::MOD_DST)) != 0;
        table = table.child(
            div()
                .h(px(22.))
                .flex()
                .items_center()
                .gap(px(SP1))
                .when_else(active, |d| d, |d| d.opacity(0.6))
                .child(mono(format!("{}", s + 1), TEXT_XS, pal.faint).w(px(14.)))
                .child(div().w(px(96.)).child(kit::pick(dev, p::slot(s, p::MOD_SRC), cx)))
                .child(div().w(px(108.)).child(kit::pick(dev, p::slot(s, p::MOD_DST), cx)))
                .child(kit::slim(ws, dev, p::slot(s, p::MOD_AMT), 60., cx)),
        );
    }
    table
}

trait WhenElse: Sized {
    fn when_else(self, cond: bool, a: impl FnOnce(Self) -> Self, b: impl FnOnce(Self) -> Self) -> Self {
        if cond { a(self) } else { b(self) }
    }
}
impl WhenElse for Div {}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let amp = adsr(ws, dev, 3, [p::AMP_ATTACK, p::AMP_DECAY, p::AMP_SUSTAIN, p::AMP_RELEASE], pal.accent, cx);
    let mut row = div()
        .flex_none()
        .h_full()
        .p(px(SP2))
        .gap(px(SP3))
        .flex()
        .child(osc(ws, dev, 0, cx))
        .child(osc(ws, dev, 1, cx))
        .child(filter(ws, dev, cx))
        .child(
            section("AMP ENVELOPE", 220.)
                .child(div().flex_1().min_h(px(40.)).child(amp))
                .child(kit::strip(
                    ws,
                    dev,
                    &[p::VOLUME, p::SUB_LEVEL, p::NOISE],
                    &[p::SUB_SHAPE, p::SUB_OCTAVE, p::VEL_SENS],
                    cx,
                )),
        );
    if dev.big {
        let env = adsr(ws, dev, 4, [p::MOD_ATTACK, p::MOD_DECAY, p::MOD_SUSTAIN, p::MOD_RELEASE], pal.accent2, cx);
        let voices = dev.m(0);
        row = row
            .child(
                section("MOD ENVELOPE", 220.)
                    .child(div().flex_1().min_h(px(40.)).child(env))
                    .child(kit::strip(ws, dev, &[p::FILTER_ENV], &[], cx)),
            )
            .child(lfo(ws, dev, 0, cx))
            .child(lfo(ws, dev, 1, cx))
            .child(matrix(ws, dev, cx))
            .child(
                section("VOICE & FX", 200.)
                    .child(div().flex().items_center().gap(px(SP1)).child(kit::pick(dev, p::VOICE_MODE, cx)).child(kit::readout("voices", format!("{voices:.0}"), pal.text)))
                    .child(kit::strip(
                        ws,
                        dev,
                        &[p::GLIDE, p::POLYPHONY, p::BEND_RANGE],
                        &[],
                        cx,
                    ))
                    .child(kit::strip(
                        ws,
                        dev,
                        &[p::CHORUS_MIX, p::DELAY_MIX, p::REVERB_MIX],
                        &[],
                        cx,
                    ))
                    .child(kit::strip(
                        ws,
                        dev,
                        &[p::DELAY_TIME, p::DELAY_FEEDBACK, p::REVERB_DECAY],
                        &[],
                        cx,
                    )),
            );
    }
    row.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_points_follow_the_parameters() {
        let p = adsr_points(0., 0., 1., 0.);
        assert_eq!(p[0], (0., 0.9));
        assert!((p[1].1 - 0.1).abs() < 1e-6 && (p[2].1 - 0.1).abs() < 1e-6, "full sustain stays at the top");
        let slow = adsr_points(1., 1., 0.25, 1.);
        assert!(slow[1].0 > p[1].0 && slow[2].0 - slow[1].0 > p[2].0 - p[1].0);
        assert!((slow[2].1 - 0.7).abs() < 1e-6, "sustain 25% is a quarter of the way up");
        assert_eq!(slow[2].1, slow[3].1);
        for w in slow.windows(2) {
            assert!(w[1].0 > w[0].0, "time runs left to right");
        }
        assert!(slow[4].0 <= 1.);
    }

    #[test]
    fn the_synth_filter_matches_its_design() {
        // 12 dB/oct, no resonance: Butterworth, −3 dB at the cutoff.
        let s = filter_sections(0, false, 1_000., 0., 48_000.);
        assert!((aitrack_dsp::response::svf_cascade_db(&s, 1_000., 48_000.) + 3.01).abs() < 0.05);
        // 24 dB/oct: two sections, −6 dB at the cutoff, resonance lifts it.
        let s = filter_sections(0, true, 1_000., 0., 48_000.);
        assert_eq!(s.len(), 2);
        let flat = aitrack_dsp::response::svf_cascade_db(&s, 1_000., 48_000.);
        assert!((flat + 3.0).abs() < 0.2, "{flat}");
        let peaky = filter_sections(0, true, 1_000., 0.8, 48_000.);
        assert!(aitrack_dsp::response::svf_cascade_db(&peaky, 1_000., 48_000.) > flat + 10.);
        // High-pass mirrors it.
        let hp = filter_sections(1, false, 1_000., 0., 48_000.);
        assert!(aitrack_dsp::response::svf_cascade_db(&hp, 100., 48_000.) < -35.);
    }
}
