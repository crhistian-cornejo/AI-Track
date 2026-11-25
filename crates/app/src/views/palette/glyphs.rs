//! A small picture of what each of our own devices looks like inside, for its
//! row in the palette: the compressor's curve, the multiband's bands, the
//! stutter's grid. Drawn from a few lines and boxes, at any size, so there is
//! nothing to render, cache or load: it is as cheap as the icon it replaces.
//! What matters is that they are told apart at a glance, not that they read.
//!
//! The shapes are plain data ([`shapes`]) in a unit box (0..1 each way, y down),
//! tested apart from drawing; [`paint`] puts them in a rectangle.

use crate::paint::{at, fill_rect, polyline};
use gpui::{Bounds, Hsla, Pixels, Window, point, px};

/// The look of a family of devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// A transfer curve bending over a knee (compressors).
    Compressor,
    Gate,
    Limiter,
    Transient,
    /// A sine cut flat at the top and bottom.
    Clipper,
    /// An S-shaped transfer curve (saturators, tape, amps).
    Saturation,
    Crusher,
    Eq,
    Filter,
    /// Three bands side by side.
    Bands,
    Spectrum,
    Reverb,
    Delay,
    Chorus,
    Autopan,
    Stutter,
    MidSide,
    Utility,
    Synth,
    Wavetable,
    Acid,
    Kick,
    Drums,
    Sampler,
    Grains,
    Vault,
    /// Notes in a roll (MIDI effects).
    Notes,
    Arp,
    Chord,
    Rack,
    Split,
}

/// The glyph for one of our devices (`fx.compressor`, `instrument.drum`…).
pub fn for_stock(id: &str) -> Glyph {
    match id {
        "fx.compressor" | "fx.buscomp" => Glyph::Compressor,
        "fx.gate" => Glyph::Gate,
        "fx.limiter" => Glyph::Limiter,
        "fx.transient" => Glyph::Transient,
        "fx.clipper" => Glyph::Clipper,
        "fx.saturator" | "fx.tape" | "fx.amp" | "fx.console" | "fx.shaper" => Glyph::Saturation,
        "fx.crusher" => Glyph::Crusher,
        "fx.eq" => Glyph::Eq,
        "fx.filter" => Glyph::Filter,
        "fx.multiband" | "fx.mbdyn" | "fx.drumbus" => Glyph::Bands,
        "fx.analyzer" => Glyph::Spectrum,
        "fx.reverb" => Glyph::Reverb,
        "fx.delay" => Glyph::Delay,
        "fx.chorus" => Glyph::Chorus,
        "fx.autopan" => Glyph::Autopan,
        "fx.stutter" => Glyph::Stutter,
        "fx.midside" => Glyph::MidSide,
        "fx.drum_extract" => Glyph::Split,
        "instrument.synth" => Glyph::Synth,
        "instrument.wavetable" => Glyph::Wavetable,
        "instrument.acid303" => Glyph::Acid,
        "instrument.kick808" => Glyph::Kick,
        "instrument.drum" | "instrument.drumsynth" | "rack.drum" => Glyph::Drums,
        "instrument.sampler" => Glyph::Sampler,
        "instrument.granular" => Glyph::Grains,
        "instrument.vault" => Glyph::Vault,
        "midi.arp" => Glyph::Arp,
        "midi.chord" => Glyph::Chord,
        id if id.starts_with("midi.") => Glyph::Notes,
        id if id.starts_with("rack.") => Glyph::Rack,
        id if id.starts_with("instrument.") => Glyph::Synth,
        _ => Glyph::Utility,
    }
}

/// One stroke or box of a glyph, in the unit box.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A line through these points (x, y).
    Line(Vec<(f32, f32)>),
    /// A box: x, y, width, height.
    Box(f32, f32, f32, f32),
    /// A box drawn fainter (grids, backgrounds).
    Faint(f32, f32, f32, f32),
}

fn curve(n: usize, f: impl Fn(f32) -> f32) -> Shape {
    Shape::Line((0..=n).map(|i| (i as f32 / n as f32, f(i as f32 / n as f32))).collect())
}

fn sine(x: f32, cycles: f32, phase: f32) -> f32 {
    0.5 - 0.38 * (std::f32::consts::TAU * (x * cycles + phase)).sin()
}

/// The shapes of `g`.
pub fn shapes(g: Glyph) -> Vec<Shape> {
    use Glyph::*;
    match g {
        Compressor => vec![
            Shape::Line(vec![(0.1, 0.9), (0.1, 0.1)]),
            Shape::Line(vec![(0.1, 0.9), (0.9, 0.9)]),
            Shape::Line(vec![(0.1, 0.9), (0.5, 0.5), (0.9, 0.3)]),
        ],
        Gate => vec![Shape::Line(vec![(0.05, 0.9), (0.4, 0.9), (0.4, 0.2), (0.7, 0.2), (0.7, 0.9), (0.95, 0.9)])],
        Limiter => vec![Shape::Faint(0.05, 0.25, 0.9, 0.0), curve(24, |x| (0.7 - 0.6 * x).max(0.25))],
        Transient => vec![Shape::Line(vec![(0.05, 0.8), (0.3, 0.8), (0.34, 0.1), (0.5, 0.6), (0.7, 0.75), (0.95, 0.8)])],
        Clipper => vec![curve(32, |x| sine(x, 1.0, 0.0).clamp(0.28, 0.72)), Shape::Faint(0.0, 0.28, 1.0, 0.0), Shape::Faint(0.0, 0.72, 1.0, 0.0)],
        Saturation => vec![Shape::Faint(0.1, 0.5, 0.8, 0.0), curve(24, |x| 0.5 - 0.4 * ((x - 0.5) * 7.0).tanh())],
        Crusher => vec![Shape::Line(vec![(0.05, 0.8), (0.2, 0.8), (0.2, 0.55), (0.35, 0.55), (0.35, 0.3), (0.5, 0.3), (0.5, 0.45), (0.65, 0.45), (0.65, 0.7), (0.8, 0.7), (0.8, 0.85), (0.95, 0.85)])],
        Eq => vec![Shape::Faint(0.05, 0.6, 0.9, 0.0), curve(32, |x| 0.6 - 0.35 * (-((x - 0.3) / 0.12).powi(2)).exp() + 0.2 * (-((x - 0.72) / 0.1).powi(2)).exp())],
        Filter => vec![curve(24, |x| if x < 0.55 { 0.3 } else { 0.3 + (x - 0.55) * 1.4 }.min(0.9)), Shape::Box(0.52, 0.18, 0.06, 0.12)],
        Bands => vec![Shape::Box(0.08, 0.35, 0.24, 0.55), Shape::Box(0.38, 0.15, 0.24, 0.75), Shape::Box(0.68, 0.5, 0.24, 0.4)],
        Spectrum => (0..9).map(|i| {
            let h = [0.3, 0.55, 0.8, 0.6, 0.7, 0.4, 0.5, 0.25, 0.15][i];
            Shape::Box(0.06 + i as f32 * 0.1, 0.9 - h, 0.07, h)
        }).collect(),
        Reverb => vec![Shape::Line(vec![(0.05, 0.9), (0.05, 0.1)]), curve(28, |x| 0.9 - 0.8 * (-(x * 4.0)).exp() * (1.0 + 0.25 * (x * 60.0).sin()).clamp(0.4, 1.0))],
        Delay => (0..5).map(|i| {
            let h = 0.8 - i as f32 * 0.15;
            Shape::Box(0.08 + i as f32 * 0.19, 0.9 - h, 0.07, h)
        }).collect(),
        Chorus => vec![curve(28, |x| sine(x, 1.5, 0.0)), curve(28, |x| sine(x, 1.5, 0.12))],
        Autopan => vec![curve(28, |x| sine(x, 1.0, 0.0)), Shape::Faint(0.02, 0.5, 0.96, 0.0)],
        Stutter => (0..8).flat_map(|i| {
            let (c, r) = (i % 4, i / 4);
            let on = [true, false, true, true, false, true, true, false][i];
            let (x, y) = (0.08 + c as f32 * 0.22, 0.2 + r as f32 * 0.4);
            if on { vec![Shape::Box(x, y, 0.18, 0.3)] } else { vec![Shape::Faint(x, y, 0.18, 0.3)] }
        }).collect(),
        MidSide => vec![
            Shape::Line(circle(0.36, 0.5, 0.3)),
            Shape::Line(circle(0.64, 0.5, 0.3)),
        ],
        Utility => vec![Shape::Line(circle(0.5, 0.5, 0.32)), Shape::Line(vec![(0.5, 0.5), (0.68, 0.28)])],
        Synth => vec![Shape::Line(vec![(0.05, 0.8), (0.3, 0.2), (0.3, 0.8), (0.55, 0.2), (0.55, 0.8), (0.8, 0.2), (0.8, 0.8), (0.95, 0.5)])],
        Wavetable => vec![
            curve(24, |x| sine(x, 1.0, 0.0) * 0.5 + 0.35),
            curve(24, |x| sine(x, 2.0, 0.0) * 0.5 + 0.2),
            curve(24, |x| sine(x, 3.0, 0.0) * 0.5 + 0.05),
        ],
        Acid => vec![
            Shape::Line(vec![(0.05, 0.75), (0.5, 0.2), (0.5, 0.75), (0.95, 0.2)]),
            Shape::Box(0.1, 0.84, 0.12, 0.08),
            Shape::Box(0.3, 0.84, 0.12, 0.08),
            Shape::Faint(0.5, 0.84, 0.12, 0.08),
            Shape::Box(0.7, 0.84, 0.12, 0.08),
        ],
        Kick => vec![curve(30, |x| 0.5 - 0.4 * (-(x * 3.2)).exp() * (x * x * 120.0 + 8.0 * x).sin())],
        Drums => (0..8).map(|i| {
            let (c, r) = (i % 4, i / 4);
            let (x, y) = (0.08 + c as f32 * 0.22, 0.16 + r as f32 * 0.42);
            if i == 1 || i == 6 { Shape::Box(x, y, 0.18, 0.32) } else { Shape::Faint(x, y, 0.18, 0.32) }
        }).collect(),
        Sampler => vec![curve(36, |x| 0.5 - 0.4 * (-(x * 2.0)).exp() * (x * 90.0).sin()), Shape::Faint(0.0, 0.5, 1.0, 0.0)],
        Grains => [(0.12, 0.3), (0.3, 0.65), (0.42, 0.2), (0.55, 0.5), (0.68, 0.78), (0.78, 0.3), (0.9, 0.55), (0.22, 0.85), (0.6, 0.15)]
            .iter()
            .map(|(x, y)| Shape::Box(*x - 0.04, *y - 0.04, 0.08, 0.08))
            .collect(),
        Vault => (0..4).map(|i| Shape::Box(0.1, 0.12 + i as f32 * 0.2, 0.8 - (i as f32 * 0.17), 0.12)).collect(),
        Notes => [(0.08, 0.7, 0.25), (0.38, 0.45, 0.2), (0.6, 0.2, 0.3), (0.2, 0.2, 0.15), (0.5, 0.75, 0.3)]
            .iter()
            .map(|(x, y, w)| Shape::Box(*x, *y, *w, 0.12))
            .collect(),
        Arp => (0..5).map(|i| Shape::Box(0.07 + i as f32 * 0.17, 0.75 - i as f32 * 0.14, 0.13, 0.12)).collect(),
        Chord => vec![Shape::Box(0.2, 0.2, 0.6, 0.12), Shape::Box(0.2, 0.44, 0.6, 0.12), Shape::Box(0.2, 0.68, 0.6, 0.12)],
        Rack => vec![Shape::Faint(0.08, 0.1, 0.84, 0.22), Shape::Box(0.08, 0.39, 0.84, 0.22), Shape::Faint(0.08, 0.68, 0.84, 0.22)],
        Split => vec![
            curve(32, |x| sine(x, 3.0, 0.0) * 0.45 + 0.05),
            curve(32, |x| 0.95 - (sine(x, 1.5, 0.2) - 0.1) * 0.45),
        ],
    }
}

fn circle(cx: f32, cy: f32, r: f32) -> Vec<(f32, f32)> {
    (0..=20).map(|i| {
        let a = i as f32 / 20.0 * std::f32::consts::TAU;
        (cx + r * a.cos(), cy + r * a.sin())
    }).collect()
}

/// Draw `g` inside `area`, in `ink`.
pub fn paint(window: &mut Window, area: Bounds<Pixels>, g: Glyph, ink: Hsla) {
    let (ox, oy) = (f32::from(area.origin.x), f32::from(area.origin.y));
    let (w, h) = (f32::from(area.size.width), f32::from(area.size.height));
    let faint = Hsla { a: ink.a * 0.35, ..ink };
    for s in shapes(g) {
        match s {
            Shape::Line(pts) => {
                let pts: Vec<_> = pts.iter().map(|(x, y)| point(px(ox + x * w), px(oy + y * h))).collect();
                polyline(window, &pts, 1.5, ink);
            }
            Shape::Box(x, y, bw, bh) => window.paint_quad(fill_rect(at(area.origin, x * w, y * h, (bw * w).max(1.), (bh * h).max(1.)), ink)),
            Shape::Faint(x, y, bw, bh) => window.paint_quad(fill_rect(at(area.origin, x * w, y * h, (bw * w).max(1.), (bh * h).max(1.)), faint)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[&str] = &[
        "fx.amp", "fx.analyzer", "fx.autopan", "fx.buscomp", "fx.chorus", "fx.clipper", "fx.compressor", "fx.console", "fx.crusher", "fx.delay", "fx.drum_extract", "fx.drumbus", "fx.eq",
        "fx.filter", "fx.gate", "fx.limiter", "fx.mbdyn", "fx.midside", "fx.multiband", "fx.reverb", "fx.saturator", "fx.shaper", "fx.stutter", "fx.tape", "fx.transient", "fx.utility",
        "instrument.acid303", "instrument.drum", "instrument.drumsynth", "instrument.granular", "instrument.kick808", "instrument.sampler", "instrument.synth", "instrument.vault", "instrument.wavetable", "midi.arp",
        "midi.chord", "midi.echo", "midi.length", "midi.pitch", "midi.random", "midi.scale", "midi.velocity", "rack.audio", "rack.drum", "rack.instrument", "rack.midi",
    ];

    #[test]
    fn every_shape_stays_inside_the_box() {
        for id in ALL {
            let g = for_stock(id);
            let shapes = shapes(g);
            assert!(!shapes.is_empty(), "{id}");
            for s in shapes {
                let pts: Vec<(f32, f32)> = match s {
                    Shape::Line(p) => p,
                    Shape::Box(x, y, w, h) | Shape::Faint(x, y, w, h) => vec![(x, y), (x + w, y + h)],
                };
                for (x, y) in pts {
                    assert!((-0.001..=1.001).contains(&x) && (-0.001..=1.001).contains(&y), "{id} {g:?}: ({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn our_devices_are_told_apart_at_a_glance() {
        // The families the owner asked about, each with its own look.
        let looks: std::collections::HashSet<_> = ["fx.compressor", "fx.multiband", "fx.stutter", "fx.eq", "fx.reverb", "fx.delay", "instrument.drum", "instrument.wavetable", "midi.arp"].iter().map(|i| for_stock(i)).collect();
        assert_eq!(looks.len(), 9);
        // Most devices have a look of their own: no more than a handful share one.
        let distinct: std::collections::HashSet<_> = ALL.iter().map(|i| for_stock(i)).collect();
        assert!(distinct.len() >= 28, "only {} looks for {} devices", distinct.len(), ALL.len());
        assert_eq!(for_stock("instrument.something_new"), Glyph::Synth, "a device added later still gets one");
    }
}
