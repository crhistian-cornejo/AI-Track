//! `fx.shaper`: the level curve of one cycle, drawn and dragged directly.
//! The two end nodes move up and down; the four in between move freely. The
//! shaded area is the gain actually applied (the curve scaled by Depth), and
//! a marker rides the curve at the position the plugin is at right now.

use super::kit::{self, Handle, PlotCfg, ax};
use super::Dev;
use crate::theme::*;
use crate::workspace::Workspace;
use aitrack_stock::fx::shaper::{curve_points, level_at, p};
use gpui::{AnyElement, Context, IntoElement, ParentElement, Styled, TextAlign, div, px};

/// The plot fraction (0 = top) of a level.
fn level_frac(level: f32) -> f32 {
    1. - level.clamp(0., 1.)
}

pub fn render(ws: &mut Workspace, dev: &Dev, cx: &mut Context<Workspace>) -> AnyElement {
    let points = curve_points(|id| dev.plain(id));
    let tension = dev.plain(p::TENSION) / 100.;
    let depth = dev.plain(p::DEPTH) / 100.;
    let default = |id: u32| (id, dev.info(id).map_or(0.5, |i| i.default_normalized()));
    // One handle per node, each bound to its own parameters (a node dragged
    // past its neighbour keeps its identity; the curve sorts them by time).
    let mut handles = vec![Handle { fx: 0., fy: level_frac(dev.plain(p::Y0)), y: ax(p::Y0, -1.), index: 0, reset: vec![default(p::Y0)], ..Default::default() }];
    for (i, (x, y)) in p::NODES.into_iter().enumerate() {
        handles.push(Handle {
            fx: dev.plain(x).clamp(0., 1.),
            fy: level_frac(dev.plain(y)),
            x: ax(x, 1.),
            y: ax(y, -1.),
            index: i + 1,
            reset: vec![default(x), default(y)],
            ..Default::default()
        });
    }
    handles.push(Handle { fx: 1., fy: level_frac(dev.plain(p::Y5)), y: ax(p::Y5, -1.), index: 5, reset: vec![default(p::Y5)], ..Default::default() });
    let marks: Vec<(f32, f32)> = handles.iter().map(|h| (h.fx, h.fy)).collect();
    let live = dev.meter.len() >= 2;
    let (phase, gain) = (dev.m(0).clamp(0., 1.), dev.m(1).clamp(0., 1.));
    let (enabled, selected) = (dev.enabled, dev.selected);
    let rate = dev.text(p::RATE);

    let plot = kit::plot(ws, dev, PlotCfg { slot: 0, handles, keys: true, ..Default::default() }, cx, move |r, window, cx| {
        let pal = p();
        let color = if enabled { pal.accent } else { pal.muted };
        // Quarters of the cycle and of the level.
        for q in 1..4 {
            kit::vline(window, r, r.px(q as f32 / 4.), content(if q == 2 { 0.12 } else { 0.06 }));
            kit::hline(window, r, r.py(q as f32 / 4.), content(0.05));
        }
        let n = ((r.w / 2.) as usize).clamp(48, 400);
        // The gain applied: the curve scaled by Depth.
        let applied = kit::sample_curve(r, n, |fx| level_frac(1. - depth * (1. - level_at(&points, tension, fx))));
        kit::fill_to(window, &applied, r.bottom(), with_alpha(color, 0.18));
        // The curve as drawn.
        let drawn = kit::sample_curve(r, n, |fx| level_frac(level_at(&points, tension, fx)));
        kit::line(window, &drawn, 2., color);
        for (i, (fx, fy)) in marks.iter().enumerate() {
            let on = i == selected;
            kit::dot(window, r.pt(*fx, *fy), if on { 7. } else { 5.5 }, if on { pal.accent } else { pal.well }, Some(if on { pal.text } else { color }));
        }
        if live {
            kit::vline(window, r, r.px(phase), with_alpha(pal.text, 0.35));
            kit::dot(window, r.pt(phase, level_frac(gain)), 4., pal.text, None);
        }
        kit::txt(window, cx, &format!("one cycle = {rate}"), r.x + 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Left);
        kit::txt(window, cx, "level", r.right() - 6., r.y + 4., TEXT_XS, pal.faint, TextAlign::Right);
    });

    kit::panel(if dev.big { 640. } else { 420. })
        .child(div().flex_1().min_h(px(0.)).rounded(px(r_md())).bg(p().well).border_1().border_color(border()).overflow_hidden().child(plot))
        .child(kit::strip(dev, &[p::RATE, p::DEPTH, p::SMOOTH, p::TENSION, p::PHASE], &[], cx))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::super::testing::dev;
    use super::*;

    #[test]
    fn handles_sit_on_the_nodes_of_the_default_curve() {
        let d = dev("fx.shaper", &[]);
        let points = curve_points(|id| d.plain(id));
        // The default is the pumping duck: silent on the beat, full by the end.
        assert_eq!(level_frac(points[0].1), 1., "start node at the bottom");
        assert_eq!(level_frac(points[5].1), 0., "end node at the top");
        assert!((level_at(&points, 0., 0.3) - d.plain(p::Y2)).abs() < 1e-5, "the curve passes through node 2");
    }
}
