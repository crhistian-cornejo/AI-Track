//! `instrument.acid303`: interactive diode-ladder filter plot + 16-step 303
//! sequencer strip (Gate / Accent / Slide / Pitch) + synthesizer controls.

use super::filter::{db_frac, paint_response};
use super::instr;
use super::kit::{self, Handle, PlotCfg, ax, freq_frac};
use super::{Dev, caption, set_now};
use crate::paint;
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode};
use aitrack_stock::instrument::acid303::{STEPS, p};
use gpui::{AnyElement, Context, Div, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled, TextAlign, div, point, prelude::FluentBuilder, px};

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    if kit::in_window() {
        return window(ws, dev, cx);
    }
    let pal = p();
    let w = if dev.big { 520. } else { 360. };
    let color = if dev.enabled { pal.accent } else { pal.faint };
    let active_step = if dev.m(1) >= 0. { Some(dev.m(1).round() as usize) } else { None };
    let filter_plot = filter_plot(ws, dev, cx);

    // Interactive 16-step sequence strip: click = Gate, Shift = Accent, Alt = Slide, right/scroll = Pitch.
    let device = dev.id;
    let mut seq = div().flex_none().flex().gap(px(3.));
    for i in 0..STEPS {
        let is_now = active_step == Some(i);
        let note = dev.plain(p::step(i, p::NOTE)).round() as i32;
        let gate = dev.choice(p::step(i, p::GATE)) == 1;
        let acc = dev.choice(p::step(i, p::S_ACCENT)) == 1;
        let slide = dev.choice(p::step(i, p::S_SLIDE)) == 1;
        let norm_note = dev.norm(p::step(i, p::NOTE));
        let bar_h = (norm_note * 18. + 4.).clamp(4., 22.);
        let cell_bg = if is_now {
            with_alpha(pal.accent, 0.30)
        } else if gate {
            content(0.08)
        } else {
            pal.well
        };
        let border_col = if is_now {
            pal.accent
        } else if acc && gate {
            with_alpha(pal.accent2, 0.8)
        } else {
            border()
        };
        let bar_col = if !gate {
            pal.faint
        } else if acc {
            pal.accent2
        } else {
            color
        };

        let mut badges = String::new();
        if acc {
            badges.push('A');
        }
        if slide {
            badges.push('S');
        }
        if badges.is_empty() {
            badges.push(if gate { '·' } else { ' ' });
        }

        let cell = div()
            .id(("acid-step", dev.key(i)))
            .flex_1()
            .h(px(if dev.big { 52. } else { 42. }))
            .rounded(px(r_sm()))
            .bg(cell_bg)
            .border_1()
            .border_color(border_col)
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .py(px(2.))
            .cursor_pointer()
            .tooltip(widgets::tip("Click: Gate · Shift: Accent · Alt: Slide · Scroll: Pitch", None))
            .child(mono(format!("{note:+}"), TEXT_XS, if gate { pal.text } else { pal.faint }))
            .child(div().w(px(8.)).h(px(bar_h)).rounded(px(2.)).bg(bar_col))
            .child(label(badges, TEXT_XS, if slide && gate { pal.accent } else { pal.muted }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let (field, cur) = if e.modifiers.shift {
                        (p::S_ACCENT, acc)
                    } else if e.modifiers.alt {
                        (p::S_SLIDE, slide)
                    } else {
                        (p::GATE, gate)
                    };
                    set_now(this, device, p::step(i, field), if cur { 0.0 } else { 1.0 });
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, e: &gpui::ScrollWheelEvent, _, cx| {
                let Some(info) = this.s.project.device(device).and_then(|_| dev_param_info(this, device, p::step(i, p::NOTE))) else {
                    return;
                };
                let cur_n = super::current(this, device, &info);
                let plain = info.to_plain(cur_n).round();
                let dy = match e.delta {
                    gpui::ScrollDelta::Lines(l) => l.y.signum(),
                    gpui::ScrollDelta::Pixels(pt) => f32::from(pt.y).signum(),
                };
                if dy.abs() > 0.1 {
                    let next = (plain + dy).clamp(info.min, info.max);
                    set_now(this, device, info.id, info.to_normalized(next));
                    cx.stop_propagation();
                    cx.notify();
                }
            }));
        seq = seq.child(cell);
    }

    let primary = [p::WAVE, p::CUTOFF, p::RESONANCE, p::ENV_MOD, p::DECAY, p::ACCENT, p::SLIDE, p::DRIVE, p::SEQ_MODE];
    let more = [p::TUNING, p::SUB, p::VOLUME];

    kit::panel(w)
        .child(
            div()
                .flex_1()
                .min_h(px(64.))
                .rounded(px(r_sm()))
                .bg(pal.well)
                .border_1()
                .border_color(border())
                .overflow_hidden()
                .child(filter_plot),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .child(caption("16-STEP PATTERN (CLICK GATE · SHIFT ACCENT · ALT SLIDE · SCROLL NOTE)"))
                .child(label(
                    if dev.choice(p::SEQ_MODE) == 1 { "Pattern Active" } else { "MIDI Input" },
                    TEXT_XS,
                    if dev.choice(p::SEQ_MODE) == 1 { pal.accent } else { pal.faint },
                )),
        )
        .child(seq)
        .child(kit::strip(ws, dev, &primary, &more, cx))
        .into_any_element()
}

/// The diode-ladder filter's response with its draggable node and the range
/// the envelope sweeps it over.
fn filter_plot(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let cutoff = dev.plain(p::CUTOFF);
    let reso = dev.plain(p::RESONANCE) / 100.;
    let env_mod = dev.plain(p::ENV_MOD) / 100.;
    let live_env = dev.m(3).clamp(0., 1.);
    let color = if dev.enabled { p().accent } else { p().faint };
    // Sketch 18 dB/oct diode response with two cascaded LP stages.
    let q1 = 0.707 + reso * 0.4;
    let q2 = 0.707 * (18.0f32 / 0.707).powf(reso);
    let secs = vec![
        SvfCoeffs::new(SvfMode::Lowpass, cutoff, q1, 0., dev.sample_rate),
        SvfCoeffs::new(SvfMode::Lowpass, cutoff, q2, 0., dev.sample_rate),
    ];
    let fx = freq_frac(cutoff);
    let fy = db_frac(aitrack_dsp::response::svf_cascade_db(&secs, cutoff as f64, dev.sample_rate) as f32).clamp(0.06, 0.94);
    let reset = [p::CUTOFF, p::RESONANCE]
        .map(|id| (id, dev.info(id).map_or(0.5, |i| i.default_normalized())))
        .to_vec();
    let handles = vec![Handle { fx, fy, x: ax(p::CUTOFF, dev.info(p::CUTOFF).map_or(1., kit::log_span)), y: ax(p::RESONANCE, -1.1), alt: ax(p::ENV_MOD, -1.0), index: 0, reset }];

    let sr = dev.sample_rate;
    let enabled = dev.enabled;
    kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        paint_response(window, cx, r, &secs, sr, color, false);
        // Env mod range band.
        if env_mod > 0.01 {
            let max_fc = (cutoff * 2.0f32.powf(env_mod * 4.8)).min(19_500.);
            let cur_fc = (cutoff * 2.0f32.powf(env_mod * 4.8 * live_env)).min(19_500.);
            let x0 = r.px(freq_frac(cutoff));
            let x1 = r.px(freq_frac(max_fc));
            if x1 > x0 + 1. {
                window.paint_quad(paint::fill_rect(paint::rect(x0, r.y, x1 - x0, r.h), with_alpha(color, 0.08)));
            }
            if live_env > 0.02 {
                let lx = r.px(freq_frac(cur_fc));
                kit::vline(window, r, lx, with_alpha(color, 0.65));
            }
        }
        let f_label = if cutoff >= 1000. { format!("{:.1}k", cutoff / 1000.) } else { format!("{cutoff:.0}") };
        kit::txt(window, cx, &f_label, r.px(fx) + 8., r.py(fy) - 10., TEXT_XS, color, TextAlign::Left);
        kit::dot(window, r.pt(fx, fy), 6., pal.well, Some(if enabled { pal.text } else { pal.faint }));
    })
    .into_any_element()
}

// ───────────────────────────── Window ─────────────────────────────

/// The note lane's range (the `Note` parameters' range) and where it sits in
/// the lane: a little room above and below the extremes.
const NOTE_LO: f32 = -12.;
const NOTE_HI: f32 = 24.;
const LANE_TOP: f32 = 0.08;
const LANE_SPAN: f32 = 0.84;
/// Width of the row names left of the lane and the pads.
const NAMES_W: f32 = 64.;

/// Vertical fraction of a note on the lane (0 = top).
pub fn note_frac(note: f32) -> f32 {
    LANE_TOP + LANE_SPAN * (1. - ((note - NOTE_LO) / (NOTE_HI - NOTE_LO)).clamp(0., 1.))
}

/// The pattern's note lane: a bar per step, dragged up and down, a line from
/// a sliding step to the next one, the running step lit.
#[allow(clippy::too_many_arguments)]
fn note_lane(ws: &mut Workspace, dev: &Dev, notes: &[f32], gates: &[bool], accents: &[bool], slides: &[bool], running: Option<usize>, cx: &mut Context<Workspace>) -> AnyElement {
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles: Vec<Handle> = (0..STEPS)
        .map(|i| {
            let id = p::step(i, p::NOTE);
            Handle { fx: (i as f32 + 0.5) / STEPS as f32, fy: note_frac(notes[i]), y: ax(id, -1. / LANE_SPAN), index: i, reset: vec![default(id)], ..Default::default() }
        })
        .collect();
    let (notes, gates, accents, slides) = (notes.to_vec(), gates.to_vec(), accents.to_vec(), slides.to_vec());
    let (color, enabled) = (if dev.enabled { p().accent } else { p().faint }, dev.enabled);
    kit::plot(ws, dev, PlotCfg { slot: 1, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let col = r.w / STEPS as f32;
        for i in (0..STEPS).step_by(8) {
            kit::bar(window, r.x + col * i as f32, r.y, col * 4., r.h, 0., content(0.025));
            kit::bar(window, r.x + col * (i + 4) as f32, r.y, col * 4., r.h, 0., content(0.0));
        }
        if let Some(i) = running {
            kit::bar(window, r.x + col * i as f32, r.y, col, r.h, 0., with_alpha(pal.accent, 0.14));
            kit::bar(window, r.x + col * i as f32, r.y, col, 3., 0., pal.accent);
        }
        for (note, text) in [(24., "+24"), (12., "+12"), (0., "0"), (-12., "-12")] {
            let y = r.py(note_frac(note));
            kit::hline(window, r, y, if note == 0. { pal.grid_bar } else { pal.grid_beat });
            kit::txt(window, cx, text, r.x + 4., y + 2., TEXT_XS, pal.faint, TextAlign::Left);
        }
        let base = r.py(note_frac(NOTE_LO));
        let centre = |i: usize| r.x + col * (i as f32 + 0.5);
        let tone = |i: usize| if !enabled || !gates[i] { pal.faint } else if accents[i] { pal.accent2 } else { color };
        for i in 0..STEPS {
            let y = r.py(note_frac(notes[i]));
            let bw = (col * 0.36).clamp(6., 24.);
            let c = tone(i);
            kit::bar(window, centre(i) - bw / 2., y, bw, (base - y).max(2.), 3., with_alpha(c, if gates[i] { 0.55 } else { 0.25 }));
        }
        for i in 0..STEPS - 1 {
            if slides[i] && gates[i] {
                let (a, b) = (point(px(centre(i)), px(r.py(note_frac(notes[i])))), point(px(centre(i + 1)), px(r.py(note_frac(notes[i + 1])))));
                kit::line(window, &[a, b], 2., pal.play);
            }
        }
        for i in 0..STEPS {
            let (x, y) = (centre(i), r.py(note_frac(notes[i])));
            kit::dot(window, point(px(x), px(y)), 5., tone(i), if running == Some(i) { Some(pal.text) } else { None });
            kit::txt(window, cx, &format!("{:+}", notes[i] as i32), x, y - 18., TEXT_XS, if gates[i] { pal.text } else { pal.faint }, TextAlign::Center);
        }
    })
    .into_any_element()
}

/// One row of 16 pads (Gate, Accent or Slide), a click flips a step.
fn pad_row(dev: &Dev, name: &str, field: u32, flags: &[bool], color: Hsla, running: Option<usize>, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let device = dev.id;
    let mut cells = div().flex_1().min_w(px(0.)).flex();
    for (i, &on) in flags.iter().enumerate() {
        let id = p::step(i, field);
        cells = cells.child(
            div().flex_1().min_w(px(0.)).p(px(2.)).child(
                div()
                    .id(("acid-pad", dev.key(0x100 | (field as usize) << 5 | i)))
                    .h(px(24.))
                    .rounded(px(r_sm()))
                    .cursor_pointer()
                    .bg(if on { with_alpha(color, 0.85) } else { content(0.05) })
                    .border_1()
                    .border_color(if running == Some(i) { pal.accent } else if on { color } else { border() })
                    .hover(|s| s.bg(if on { color } else { content(0.12) }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            set_now(this, device, id, if on { 0. } else { 1. });
                            cx.notify();
                        }),
                    ),
            ),
        );
    }
    div().flex_none().flex().items_center().child(div().flex_none().w(px(NAMES_W)).child(caption(name.to_owned()))).child(cells)
}

/// The 16-step pattern: notes on a lane, then the pad rows.
fn pattern(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> Div {
    let pal = p();
    let running = if dev.m(1) >= 0. { Some(dev.m(1).round() as usize) } else { None };
    let notes: Vec<f32> = (0..STEPS).map(|i| dev.plain(p::step(i, p::NOTE)).round()).collect();
    let flag = |field: u32| -> Vec<bool> { (0..STEPS).map(|i| dev.choice(p::step(i, field)) == 1).collect() };
    let (gates, accents, slides) = (flag(p::GATE), flag(p::S_ACCENT), flag(p::S_SLIDE));
    let on = dev.choice(p::SEQ_MODE) == 1;
    let lane = note_lane(ws, dev, &notes, &gates, &accents, &slides, running, cx);
    let numbers = div().flex_1().min_w(px(0.)).flex().children((0..STEPS).map(|i| {
        let c = if running == Some(i) { pal.accent } else if i % 4 == 0 { pal.muted } else { pal.faint };
        div().flex_1().min_w(px(0.)).flex().justify_center().child(mono(format!("{}", i + 1), TEXT_XS, c))
    }));
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(SP1))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SP3))
                .child(caption("PATTERN"))
                .child(label(if on { "Running with the transport. Notes you play transpose it." } else { "Off: the synth plays the notes you send it. Switch to Pattern to run these." }, TEXT_XS, if on { pal.accent } else { pal.faint }))
                .child(div().flex_1())
                .child(instr::chips(dev, p::SEQ_MODE, cx)),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(SP1))
                .when(!on, |d| d.opacity(0.6))
                .child(div().flex().child(div().flex_none().w(px(NAMES_W)).child(caption("NOTE"))).child(instr::stage(112.).h(px(128.)).child(lane)))
                .child(div().flex().child(div().flex_none().w(px(NAMES_W))).child(numbers))
                .child(pad_row(dev, "GATE", p::GATE, &gates, pal.accent, running, cx))
                .child(pad_row(dev, "ACCENT", p::S_ACCENT, &accents, pal.accent2, running, cx))
                .child(pad_row(dev, "SLIDE", p::S_SLIDE, &slides, pal.play, running, cx)),
        )
        .child(instr::hint("Drag a bar to set its note (Shift: fine) · click a pad to switch it · a slide glides into the next step"))
}

fn window(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let (cutoff, reso, env_mod) = (dev.plain(p::CUTOFF), dev.plain(p::RESONANCE), dev.plain(p::ENV_MOD));
    let live = dev.m(0) > 0.5;
    let color = if dev.enabled { pal.accent } else { pal.faint };
    let cut = if cutoff >= 1000. { format!("{:.2} kHz", cutoff / 1000.) } else { format!("{cutoff:.0} Hz") };
    let plot = filter_plot(ws, dev, cx);
    let controls = instr::groups(vec![
        instr::group("OSCILLATOR", color, instr::dials(dev, &[p::WAVE, p::TUNING, p::SUB], cx)),
        instr::group("FILTER", pal.accent2, instr::dials(dev, &[p::CUTOFF, p::RESONANCE, p::ENV_MOD, p::DECAY], cx)),
        instr::group("DYNAMICS", color, instr::dials(dev, &[p::ACCENT, p::SLIDE, p::DRIVE], cx)),
        instr::group("OUTPUT", pal.muted, instr::dials(dev, &[p::VOLUME], cx)),
    ]);
    instr::root(900.)
        .child(instr::title(
            "ACID 303 · DIODE LADDER FILTER",
            vec![
                instr::tag("CUTOFF", cut, color, false),
                instr::tag("RESO", format!("{reso:.0}%"), color, false),
                instr::tag("ENV MOD", format!("{env_mod:.0}%"), pal.accent2, live),
            ],
        ))
        .child(instr::stage(120.).child(plot))
        .child(controls)
        .child(pattern(ws, dev, cx))
        .into_any_element()
}

fn dev_param_info(ws: &Workspace, device: aitrack_model::DeviceId, id: u32) -> Option<aitrack_dsp::ParamInfo> {
    let d = ws.s.project.device(device)?;
    let key = match &d.kind {
        aitrack_model::DeviceKind::Stock { plugin_id } => plugin_id.as_str(),
        _ => return None,
    };
    ws.b.plugins.device(key)?.params.iter().find(|p| p.id == id).cloned()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn acid_editor_reads_filter_and_steps() {
        let d = dev("instrument.acid303", &[(p::CUTOFF, 900.), (p::step(0, p::NOTE), 12.)]);
        assert!((d.plain(p::CUTOFF) - 900.).abs() < 2.0);
        assert!((d.plain(p::step(0, p::NOTE)) - 12.).abs() < 0.01);
        assert_eq!(d.choice(p::step(0, p::GATE)), 1);
    }

    #[test]
    fn the_note_lane_covers_the_notes_the_steps_play() {
        let d = dev("instrument.acid303", &[]);
        let info = d.info(p::step(0, p::NOTE)).unwrap();
        assert_eq!((info.min, info.max), (NOTE_LO, NOTE_HI));
        assert!(note_frac(NOTE_HI) < note_frac(0.) && note_frac(0.) < note_frac(NOTE_LO), "high notes are at the top");
        // One step of the lane is a step of the parameter: the handle axis is the lane's span.
        let per_step = (note_frac(0.) - note_frac(1.)) * (NOTE_HI - NOTE_LO);
        assert!((per_step - LANE_SPAN).abs() < 1e-4);
    }
}
