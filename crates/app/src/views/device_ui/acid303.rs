//! `instrument.acid303`: interactive diode-ladder filter plot + 16-step 303
//! sequencer strip (Gate / Accent / Slide / Pitch) + synthesizer controls.

use super::filter::{db_frac, paint_response};
use super::kit::{self, Handle, PlotCfg, ax, freq_frac};
use super::{Dev, caption, set_now};
use crate::paint;
use crate::theme::*;
use crate::widgets::{self, label, mono};
use crate::workspace::Workspace;
use aitrack_dsp::svf::{SvfCoeffs, SvfMode};
use aitrack_stock::instrument::acid303::{STEPS, p};
use gpui::{AnyElement, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled, TextAlign, div, px};

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let pal = p();
    let w = if dev.big { 520. } else { 360. };
    let cutoff = dev.plain(p::CUTOFF);
    let reso = dev.plain(p::RESONANCE) / 100.;
    let env_mod = dev.plain(p::ENV_MOD) / 100.;
    let live_env = dev.m(3).clamp(0., 1.);
    let active_step = if dev.m(1) >= 0. { Some(dev.m(1).round() as usize) } else { None };
    let color = if dev.enabled { pal.accent } else { pal.faint };

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
    let handles = vec![Handle { fx, fy, x: ax(p::CUTOFF, 1.), y: ax(p::RESONANCE, -1.1), alt: ax(p::ENV_MOD, -1.0), index: 0, reset }];

    let sr = dev.sample_rate;
    let enabled = dev.enabled;
    let filter_plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
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
    });

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
}
