//! `fx.delay`: the taps on a beat grid (left channel above the line, right
//! below, fading by the feedback) with draggable first taps and feedback,
//! and the feedback filter as a draggable range on a frequency bar.

use super::kit::{self, Handle, PlotCfg, ax, freq_frac};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::lfo::division_beats;
use aitrack_stock::fx::delay::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// One echo: `(beats after the dry sound, level 0..1, right channel)`.
pub type Tap = (f32, f32, bool);

/// The echoes of a delay with these settings, up to `span` beats.
pub fn taps(mode: usize, left: f32, right: f32, feedback: f32, span: f32) -> Vec<Tap> {
    let mut out = Vec::new();
    let lane = |time: f32, is_right: bool, out: &mut Vec<Tap>| {
        let (mut t, mut level) = (time, 1.0f32);
        while t <= span && level > 0.02 && out.len() < 96 {
            out.push((t, level, is_right));
            t += time;
            level *= feedback;
        }
    };
    match mode {
        // Ping-pong: one line bouncing between the channels.
        1 => {
            let (mut t, mut level, mut side) = (left, 1.0f32, false);
            while t <= span && level > 0.02 && out.len() < 96 {
                out.push((t, level, side));
                t += left;
                level *= feedback;
                side = !side;
            }
        }
        2 => {
            lane(left, false, &mut out);
            lane(left, true, &mut out);
        }
        _ => {
            lane(left, false, &mut out);
            lane(right, true, &mut out);
        }
    }
    out
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let sync = dev.on(p::SYNC);
    let beats = |time: u32, div: u32| -> f32 {
        if sync { division_beats(dev.choice(div)) as f32 } else { dev.plain(time) / 1000. * dev.bpm as f32 / 60. }
    };
    let (left, right) = (beats(p::TIME_L, p::DIV_L).max(0.01), beats(p::TIME_R, p::DIV_R).max(0.01));
    let (mode, feedback) = (dev.choice(p::MODE), dev.plain(p::FEEDBACK) / 100.);
    let span = (left.max(right) * 4.).clamp(4., 16.).ceil();
    let echoes = taps(mode, left, right, feedback, span);
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    // Synced, a tap steps through the divisions (longer to the right).
    let time_axis = |time: u32, div: u32| if sync { ax(div, -1.2) } else { ax(time, 1.2) };
    let reset = |time: u32, div: u32| vec![default(if sync { div } else { time })];
    let mut handles = vec![
        Handle { fx: (left / span).min(1.), fy: 0.08, x: time_axis(p::TIME_L, p::DIV_L), index: 0, reset: reset(p::TIME_L, p::DIV_L), ..Default::default() },
        Handle { fx: (2. * left / span).min(1.), fy: 0.5 - 0.42 * feedback, y: ax(p::FEEDBACK, -1. / 0.42), index: 2, reset: vec![default(p::FEEDBACK)], ..Default::default() },
    ];
    if mode == 0 {
        handles.push(Handle { fx: (right / span).min(1.), fy: 0.92, x: time_axis(p::TIME_R, p::DIV_R), index: 1, reset: reset(p::TIME_R, p::DIV_R), ..Default::default() });
    }
    let (marks, enabled, wet) = (handles.clone(), dev.enabled, [dev.m(0), dev.m(1)]);
    let timeline = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        for b in 0..=span as usize {
            let x = r.px(b as f32 / span);
            kit::vline(window, r, x, if b % 4 == 0 { pal.grid_bar } else { pal.grid_beat });
            if b % 4 == 0 || span <= 8. {
                kit::txt(window, cx, &format!("{b}"), x + 3., r.py(0.5) - 6., TEXT_XS, pal.faint, TextAlign::Left);
            }
        }
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        kit::txt(window, cx, "L", r.x + 4., r.y + 3., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "R", r.x + 4., r.bottom() - 15., TEXT_XS, pal.faint, TextAlign::Left);
        // The dry hit, then the echoes fading by the feedback.
        kit::bar(window, r.x + 1., r.py(0.08), 3., r.h * 0.84, 1.5, pal.text);
        for (t, level, is_right) in &echoes {
            let color = if !enabled { pal.muted } else if *is_right { series(1) } else { series(0) };
            let h = r.h * 0.42 * level;
            let x = r.px(t / span) - 2.;
            kit::bar(window, x, if *is_right { r.py(0.5) } else { r.py(0.5) - h }, 4., h, 2., with_alpha(color, 0.35 + 0.65 * level));
        }
        // What the wet signal is doing now.
        for (i, level) in wet.iter().enumerate() {
            let h = (level.clamp(0., 1.) * r.h * 0.42).max(0.);
            kit::bar(window, r.right() - 5., if i == 1 { r.py(0.5) } else { r.py(0.5) - h }, 3., h, 1.5, pal.accent2);
        }
        for h in &marks {
            kit::dot(window, r.pt(h.fx, h.fy), 6., if h.index == 1 { series(1) } else if h.index == 2 { pal.text } else { series(0) }, Some(pal.raised));
        }
    });

    // The feedback filter's pass band.
    let (lo, hi) = (dev.plain(p::LOW_CUT), dev.plain(p::HIGH_CUT));
    let span_of = |id: u32| dev.info(id).map_or(1., kit::log_span);
    let handles = vec![
        Handle { fx: freq_frac(lo), fy: 0.5, x: ax(p::LOW_CUT, span_of(p::LOW_CUT)), index: 3, reset: vec![default(p::LOW_CUT)], ..Default::default() },
        Handle { fx: freq_frac(hi), fy: 0.5, x: ax(p::HIGH_CUT, span_of(p::HIGH_CUT)), index: 4, reset: vec![default(p::HIGH_CUT)], ..Default::default() },
    ];
    let filter = kit::plot(ws, dev, PlotCfg { slot: 1, handles, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::freq_grid(window, cx, r, false);
        let (a, b) = (r.px(freq_frac(lo)), r.px(freq_frac(hi)));
        kit::bar(window, a.min(b), r.y + 6., (b - a).abs(), r.h - 12., 3., accent_alpha(0.3));
        kit::txt(window, cx, "feedback filter", (a + b) / 2., r.py(0.5) - 6., TEXT_XS, pal.muted, TextAlign::Center);
        kit::dot(window, r.pt(freq_frac(lo), 0.5), 6., pal.accent, Some(pal.text));
        kit::dot(window, r.pt(freq_frac(hi), 0.5), 6., pal.accent, Some(pal.text));
    });

    let (tl, tr) = if sync { (p::DIV_L, p::DIV_R) } else { (p::TIME_L, p::TIME_R) };
    kit::panel(if dev.big { 720. } else { 500. })
        .child(div().flex_1().min_h(px(0.)).child(timeline))
        .child(div().flex_none().h(px(28.)).child(filter))
        .child(
            div()
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(SP1))
                .child(kit::pick(dev, p::MODE, cx))
                .child(kit::pick(dev, p::SYNC, cx))
                .child(kit::strip(
                    ws,
                    dev,
                    &[tl, tr, p::FEEDBACK, p::MIX, p::DUCK],
                    &[
                        p::LOW_CUT,
                        p::HIGH_CUT,
                        p::MOD_RATE,
                        p::MOD_DEPTH,
                        p::WIDTH,
                        p::DRIVE,
                        p::OUTPUT,
                    ],
                    cx,
                )),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taps_fade_by_the_feedback() {
        let t = taps(0, 0.75, 1., 0.5, 4.);
        let left: Vec<&Tap> = t.iter().filter(|t| !t.2).collect();
        assert_eq!(left.len(), 5);
        assert_eq!(*left[0], (0.75, 1., false));
        assert!((left[2].1 - 0.25).abs() < 1e-6 && (left[2].0 - 2.25).abs() < 1e-6);
        assert_eq!(t.iter().filter(|t| t.2).count(), 4, "the right line runs at its own time");
        // No feedback: one echo per side.
        assert_eq!(taps(0, 0.5, 0.5, 0., 4.).len(), 2);
    }

    #[test]
    fn ping_pong_alternates_and_mono_doubles() {
        let t = taps(1, 0.5, 9., 0.8, 2.);
        assert_eq!(t.iter().map(|t| t.2).collect::<Vec<_>>(), vec![false, true, false, true]);
        let m = taps(2, 1., 9., 0.5, 2.);
        assert_eq!(m.len(), 4);
        assert_eq!(m.iter().filter(|t| t.2).count(), 2);
        assert!(taps(0, 0.01, 0.01, 0.99, 16.).len() <= 96, "dense settings stay bounded");
    }
}
