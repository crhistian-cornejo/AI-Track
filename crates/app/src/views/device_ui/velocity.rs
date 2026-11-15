//! `midi.velocity`: the curve from the velocity played (across) to the one
//! that comes out (up), as the device computes it: its curve, compression or
//! expansion around the pivot, gain and mode. The band around it is the
//! Random spread, the shaded strips above Max and below Min the range the
//! output is held to, and the vertical line the pivot. Drag the pivot dot
//! (across: Pivot, up or down: Compand), the dot on the curve (Curve) and the
//! two range dots (Min and Max). The device has no meter, so there are no
//! live points: notes coming in are not reported.

use super::kit::{self, Handle, PlotCfg, ax};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::midi::velocity::{Shape, p};
use gpui::{AnyElement, Context, InteractiveElement, ParentElement, Styled, TextAlign, div, px};

/// The plot fraction of a velocity 1..127 across (and, flipped, up).
fn frac(v: f32) -> f32 {
    ((v - 1.) / 126.).clamp(0., 1.)
}

/// Room left around the plot so the handles at the ends are not cut.
const PAD: f32 = 0.07;

/// Plot fractions across and up (0 = top) of a velocity, inside the padding.
fn gx(v: f32) -> f32 {
    PAD + (1. - 2. * PAD) * frac(v)
}

fn gy(v: f32) -> f32 {
    PAD + (1. - 2. * PAD) * (1. - frac(v))
}

/// The device's shape for its parameters (the same arithmetic as the device).
pub fn shape_of(dev: &Dev) -> Shape {
    let curve = dev.plain(p::CURVE) / 100.;
    let compand = dev.plain(p::COMPAND) / 100.;
    let (a, b) = (dev.plain(p::MIN), dev.plain(p::MAX));
    Shape {
        mode: dev.choice(p::MODE).min(2),
        fixed: dev.plain(p::FIXED),
        gamma: (-1.5 * curve).exp2(),
        factor: if compand >= 0. { 1. + 2. * compand } else { 1. + compand },
        pivot: dev.plain(p::PIVOT),
        gain: dev.plain(p::GAIN),
        min: a.min(b),
        max: a.max(b),
        random: dev.plain(p::RANDOM) / 100.,
        seed: dev.plain(p::SEED) as u64,
    }
}

/// The output velocity for an input one (1..127), with no random draw.
pub fn out_of(shape: &Shape, input: f32) -> f32 {
    shape.apply(input / 127., 0.)
}

/// How far (velocity steps) the random draw can move an output either way.
pub fn spread(shape: &Shape) -> f32 {
    shape.random * 0.5 * 127.
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let shape = shape_of(dev);
    let enabled = dev.enabled;
    let fixed = shape.mode == 2;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));

    let at_pivot = out_of(&shape, shape.pivot);
    let at_curve = out_of(&shape, 32.);
    let handles = vec![
        Handle { fx: gx(shape.pivot), fy: gy(at_pivot), x: ax(p::PIVOT, 1. / (1. - 2. * PAD)), y: ax(p::COMPAND, -1. / (1. - 2. * PAD)), index: 0, reset: vec![default(p::PIVOT), default(p::COMPAND)], ..Default::default() },
        Handle { fx: gx(32.), fy: gy(at_curve), y: ax(p::CURVE, -1. / (1. - 2. * PAD)), index: 1, reset: vec![default(p::CURVE)], ..Default::default() },
        Handle { fx: 0.96, fy: gy(shape.max), y: ax(p::MAX, -1. / (1. - 2. * PAD)), index: 2, reset: vec![default(p::MAX)], ..Default::default() },
        Handle { fx: 0.04, fy: gy(shape.min), y: ax(p::MIN, -1. / (1. - 2. * PAD)), index: 3, reset: vec![default(p::MIN)], ..Default::default() },
    ];
    let marks: Vec<(f32, f32)> = handles.iter().map(|h| (h.fx, h.fy)).collect();
    let title = if fixed { format!("every note comes out at {:.0}", out_of(&shape, 64.)) } else { format!("range {:.0}–{:.0} · pivot {:.0}", shape.min, shape.max, shape.pivot) };

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let colour = if enabled { pal.accent } else { pal.muted };
        for q in [0.25, 0.5, 0.75] {
            kit::vline(window, r, r.px(q), pal.grid_beat);
            kit::hline(window, r, r.py(q), pal.grid_beat);
        }
        // What the range cuts off.
        let (top, bottom) = (r.py(gy(shape.max)), r.py(gy(shape.min)));
        kit::bar(window, r.x, r.y, r.w, (top - r.y).max(0.), 0., content(0.06));
        kit::bar(window, r.x, bottom, r.w, (r.bottom() - bottom).max(0.), 0., content(0.06));
        kit::hline(window, r, top, pal.grid_bar);
        kit::hline(window, r, bottom, pal.grid_bar);
        // Straight through, for reference, and the pivot.
        kit::line(window, &[r.pt(gx(1.), gy(1.)), r.pt(gx(127.), gy(127.))], 1., content(0.18));
        kit::vline(window, r, r.px(gx(shape.pivot)), content(0.14));
        // The curve, with the random spread around it.
        let n = ((r.w / 2.) as usize).clamp(64, 252);
        let at = |i: usize| 1. + 126. * i as f32 / (n - 1) as f32;
        let fx_of = |i: usize| gx(at(i));
        let centre: Vec<gpui::Point<gpui::Pixels>> = (0..n).map(|i| r.pt(fx_of(i), gy(out_of(&shape, at(i))))).collect();
        if shape.random > 0. && !fixed {
            let s = spread(&shape);
            let up: Vec<gpui::Point<gpui::Pixels>> = (0..n).map(|i| r.pt(fx_of(i), gy((out_of(&shape, at(i)) + s).min(shape.max.max(shape.min))))).collect();
            let down: Vec<gpui::Point<gpui::Pixels>> = (0..n).map(|i| r.pt(fx_of(i), gy((out_of(&shape, at(i)) - s).max(shape.min.min(shape.max))))).collect();
            kit::fill_to(window, &up, r.bottom(), with_alpha(colour, 0.12));
            kit::fill_to(window, &down, r.bottom(), pal.well);
            kit::line(window, &up, 1., with_alpha(colour, 0.35));
            kit::line(window, &down, 1., with_alpha(colour, 0.35));
        } else {
            kit::fill_to(window, &centre, r.bottom(), with_alpha(colour, 0.12));
        }
        kit::line(window, &centre, 2., colour);
        for (i, (fx, fy)) in marks.iter().enumerate() {
            let c = if i == 0 { pal.accent2 } else { colour };
            kit::dot(window, r.pt(*fx, *fy), if i == 0 { 6.5 } else { 5.5 }, c, Some(pal.text));
        }
        kit::txt(window, cx, &title, r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "played →", r.right() - 6., r.bottom() - 16., TEXT_XS, pal.faint, TextAlign::Right);
        kit::txt(window, cx, "out ↑", r.x + 6., r.y + 18., TEXT_XS, pal.faint, TextAlign::Left);
    });

    let mut controls = vec![p::CURVE, p::COMPAND, p::PIVOT, p::GAIN, p::MIN, p::MAX, p::RANDOM];
    if fixed {
        controls.insert(0, p::FIXED);
    }
    if shape.random > 0. {
        controls.push(p::SEED);
    }
    let frame = div().id(("velocity-plot", dev.key(0))).size_full().rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(plot);
    let controls = div().flex_none().flex().flex_wrap().items_center().gap(px(SP1)).child(kit::pick(dev, p::MODE, cx)).child(kit::strip(ws, dev, &controls, &[], cx));
    super::midi_kit::body_with(dev, "velocity-body", 520., 100., 192., frame, vec![controls])
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;
    use aitrack_stock::midi::velocity::Velocity;

    #[test]
    fn the_default_is_the_straight_line_and_matches_the_device() {
        let d = dev("midi.velocity", &[]);
        let s = shape_of(&d);
        assert_eq!(s, Velocity::new().shape());
        for v in [1., 32., 64., 100., 127.] {
            assert_eq!(out_of(&s, v), v, "straight through at {v}");
        }
    }

    #[test]
    fn the_curve_compand_range_and_fixed_modes_bend_it_as_the_device_does() {
        // A positive curve lifts soft notes, a negative one drops them.
        assert!(out_of(&shape_of(&dev("midi.velocity", &[(p::CURVE, 60.)])), 32.) > 32.);
        assert!(out_of(&shape_of(&dev("midi.velocity", &[(p::CURVE, -60.)])), 32.) < 32.);
        // Full compression puts everything on the pivot.
        let squash = shape_of(&dev("midi.velocity", &[(p::COMPAND, -100.), (p::PIVOT, 80.)]));
        assert!((out_of(&squash, 10.) - 80.).abs() <= 1. && (out_of(&squash, 120.) - 80.).abs() <= 1.);
        // Clip keeps the velocity inside Min..Max.
        let clip = shape_of(&dev("midi.velocity", &[(p::MODE, 1.), (p::MIN, 40.), (p::MAX, 90.)]));
        assert_eq!((out_of(&clip, 10.), out_of(&clip, 127.), out_of(&clip, 60.)), (40., 90., 60.));
        // Fixed ignores the input.
        let fixed = shape_of(&dev("midi.velocity", &[(p::MODE, 2.), (p::FIXED, 90.)]));
        assert_eq!((out_of(&fixed, 5.), out_of(&fixed, 120.)), (90., 90.));
        // Swapped Min and Max still make a range.
        let swapped = shape_of(&dev("midi.velocity", &[(p::MIN, 100.), (p::MAX, 20.)]));
        assert_eq!((swapped.min, swapped.max), (20., 100.));
    }

    #[test]
    fn the_random_spread_is_half_of_it_each_way_and_the_plot_maps_the_velocity_range() {
        assert_eq!(spread(&shape_of(&dev("midi.velocity", &[(p::RANDOM, 50.)]))), 31.75);
        assert_eq!((frac(1.), frac(127.), frac(64.)), (0., 1., 0.5));
    }
}
