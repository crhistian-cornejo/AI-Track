//! `fx.chorus`: the modulation each voice follows over one LFO cycle (drag
//! the handle: depth up/down, rate left/right), with the plugin's LFO phase
//! running through it.

use super::kit::{self, Handle, PlotCfg, ax};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_dsp::lfo::LfoShape;
use aitrack_stock::fx::chorus::p;
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};
use std::f32::consts::TAU;

/// One cycle of an LFO shape at `phase` (0..1), −1..1. The random shapes
/// are drawn with a fixed pattern so the picture is steady.
pub fn lfo_value(shape: LfoShape, phase: f32) -> f32 {
    let ph = phase.rem_euclid(1.);
    let steps = [0.3, -0.7, 0.9, -0.2, 0.6, -0.9, 0.1, -0.4];
    match shape {
        LfoShape::Sine => (TAU * ph).sin(),
        LfoShape::Triangle => tri(ph),
        LfoShape::SawUp => 2. * ph - 1.,
        LfoShape::SawDown => 1. - 2. * ph,
        LfoShape::Square => if ph < 0.5 { 1. } else { -1. },
        LfoShape::SampleHold => steps[(ph * 8.) as usize % 8],
        LfoShape::SmoothRandom => {
            let x = ph * 8.;
            let (a, b) = (steps[x as usize % 8], steps[(x as usize + 1) % 8]);
            let t = x.fract();
            a + (b - a) * t * t * (3. - 2. * t)
        }
    }
}

fn tri(ph: f32) -> f32 {
    if ph < 0.25 {
        4. * ph
    } else if ph < 0.75 {
        2. - 4. * ph
    } else {
        4. * ph - 4.
    }
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let mode = dev.choice(p::MODE);
    let depth = dev.plain(p::DEPTH) / 100.;
    // Chorus voices are spread around the cycle; flanger and phaser run one
    // line per channel, offset by the stereo spread.
    let voices = if mode == 0 { dev.choice(p::VOICES) + 2 } else { 2 };
    let spread = dev.plain(p::SPREAD) / 100.;
    let offsets: Vec<f32> = (0..voices).map(|v| if mode == 0 { v as f32 / voices as f32 } else { v as f32 * 0.5 * spread }).collect();
    let sync = dev.on(p::SYNC);
    let rate_axis = if sync { ax(p::DIVISION, -1.2) } else { ax(p::RATE, 1.2) };
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    let handles = vec![Handle { fx: 0.25, fy: 0.5 - 0.42 * depth, x: rate_axis, y: ax(p::DEPTH, -1. / 0.42), index: 0, reset: vec![default(p::DEPTH)], ..Default::default() }];
    let (phase, enabled) = (dev.m(0), dev.enabled);
    let title = match mode {
        0 => format!("{voices} voices around {}", dev.text(p::DELAY)),
        1 => format!("flanger around {}", dev.text(p::DELAY)),
        _ => format!("{} stages around {}", dev.text(p::STAGES), dev.text(p::CENTER)),
    };
    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        kit::hline(window, r, r.py(0.5), pal.grid_bar);
        for q in [0.25, 0.5, 0.75] {
            kit::vline(window, r, r.px(q), pal.grid_beat);
        }
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        let y = move |ph: f32, off: f32| 0.5 - 0.42 * depth * (TAU * (ph + off)).sin();
        for (v, off) in offsets.iter().enumerate() {
            let color = if enabled { series(v) } else { pal.muted };
            let off = *off;
            kit::line(window, &kit::sample_curve(r, 96, |fx| y(fx, off)), 1.5, with_alpha(color, 0.85));
            kit::dot(window, r.pt(phase.rem_euclid(1.), y(phase, off)), 3.5, color, None);
        }
        kit::vline(window, r, r.px(phase.rem_euclid(1.)), with_alpha(pal.accent2, 0.5));
        kit::dot(window, r.pt(0.25, 0.5 - 0.42 * depth), 7., pal.accent, Some(pal.text));
    });
    let rate = if sync { p::DIVISION } else { p::RATE };
    let voicing = match mode {
        0 => vec![p::VOICES, p::DELAY],
        1 => vec![p::DELAY],
        _ => vec![p::STAGES, p::CENTER],
    };
    let mut primary = vec![rate, p::DEPTH, p::FEEDBACK, p::MIX];
    primary.extend(voicing);
    kit::panel(if dev.big { 600. } else { 420. })
        .child(div().flex_1().min_h(px(0.)).child(plot))
        .child(div().flex_none().flex().flex_wrap().items_center().gap(px(SP1)).child(kit::pick(dev, p::MODE, cx)).child(kit::pick(dev, p::SYNC, cx)).child(kit::strip(dev, &primary, &[p::SPREAD, p::OUTPUT], cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lfo_shapes_stay_in_range_and_have_their_landmarks() {
        for shape in LfoShape::ALL {
            for i in 0..=64 {
                let v = lfo_value(shape, i as f32 / 64.);
                assert!((-1.0001..=1.0001).contains(&v), "{shape:?} at {i}: {v}");
            }
        }
        assert!((lfo_value(LfoShape::Sine, 0.25) - 1.).abs() < 1e-5);
        assert!((lfo_value(LfoShape::Triangle, 0.25) - 1.).abs() < 1e-5);
        assert!((lfo_value(LfoShape::Triangle, 0.75) + 1.).abs() < 1e-5);
        assert_eq!(lfo_value(LfoShape::Triangle, 0.), 0.);
        assert_eq!(lfo_value(LfoShape::SawUp, 0.5), 0.);
        assert_eq!(lfo_value(LfoShape::Square, 0.75), -1.);
    }
}
