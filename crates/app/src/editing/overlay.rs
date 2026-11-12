//! What the arrangement and the piano roll paint for editing: the adaptive
//! grid, the ruler's ticks and labels, the insert marker, the time selection
//! and the notches where a looping clip repeats.
//!
//! Each `Overlay` is gathered while the view renders and painted from its
//! canvas, so the canvases stay plain data → pixels.

use super::GridView;
use crate::grid::{GridSpec, Line};
use crate::paint::{self, at};
use crate::state::AppState;
use crate::theme::*;
use aitrack_model::*;
use gpui::{App, FontWeight, Hsla, Pixels, Point, TextAlign, Window, point, px};

/// Grid lines closer than this are not drawn.
const MIN_LINE_PX: f32 = 3.;

/// The time selection on screen: its edges in pixels from the left of the
/// canvas, and the (top, height) of each lane it covers.
#[derive(Clone, Debug)]
struct TimeBand {
    left: f32,
    right: f32,
    lanes: Vec<(f32, f32)>,
}

/// The grid and selection of one view at one zoom.
#[derive(Clone, Debug)]
pub struct Overlay {
    spec: GridSpec,
    sig: TimeSignature,
    px_per_tick: f32,
    scroll_x: f32,
    /// Distance between ruler labels.
    label_step: Tick,
    /// The insert marker, in pixels from the left edge of the canvas.
    marker: f32,
    time: Option<TimeBand>,
}

impl Overlay {
    /// The arrangement's overlay. `lanes` are the rows on screen:
    /// (track, top in pixels from the top of the canvas, height).
    pub fn arrange(s: &AppState, lanes: &[(TrackId, f32, f32)]) -> Self {
        let view = s.arrange;
        let px_per_tick = view.px_per_beat / PPQ as f32;
        let sig = s.project.tempo.signature_at(0);
        let x = |t: Tick| t as f32 * px_per_tick - view.scroll_x;
        let time = s.time_selection().map(|sel| {
            let lanes = lanes.iter().filter(|(t, ..)| sel.tracks.contains(t)).map(|(_, top, h)| (*top, *h)).collect();
            TimeBand { left: x(sel.start), right: x(sel.end), lanes }
        });
        Self {
            spec: s.draw_spec(GridView::Arrange),
            sig,
            px_per_tick,
            scroll_x: view.scroll_x,
            label_step: crate::nav::label_step(view.px_per_beat, sig),
            marker: x(s.ed.marker),
            time,
        }
    }

    /// The piano roll's overlay (its marker is in clip time).
    pub fn piano(s: &AppState) -> Self {
        let view = s.piano;
        let px_per_tick = view.px_per_beat / PPQ as f32;
        let sig = s.project.tempo.signature_at(0);
        Self {
            spec: s.draw_spec(GridView::Piano),
            sig,
            px_per_tick,
            scroll_x: view.scroll_x,
            label_step: crate::nav::label_step(view.px_per_beat, sig),
            marker: s.ed.piano_marker as f32 * px_per_tick - view.scroll_x,
            time: None,
        }
    }

    fn x(&self, tick: Tick) -> f32 {
        tick as f32 * self.px_per_tick - self.scroll_x
    }

    /// The ticks of the multiples of `step` across a canvas `w` px wide.
    fn ticks(&self, step: Tick, w: f32) -> impl Iterator<Item = Tick> {
        let step = step.max(1);
        let first = ((self.scroll_x / self.px_per_tick) as Tick).div_euclid(step);
        let last = (((self.scroll_x + w) / self.px_per_tick) as Tick).div_euclid(step) + 1;
        (first..=last).map(move |i| i * step)
    }

    /// The vertical grid lines: bars, beats and subdivisions, each weaker
    /// than the last. Every other group of four bars is shaded when `shade`.
    pub fn paint_grid(&self, window: &mut Window, o: Point<Pixels>, w: f32, h: f32, shade: bool) {
        let pal = p();
        let bar_px = self.spec.bar as f32 * self.px_per_tick;
        if shade {
            let group = self.spec.bar * 4;
            for t in self.ticks(group * 2, w) {
                window.paint_quad(paint::fill_rect(at(o, self.x(t + group), 0., bar_px * 4., h), content(0.012)));
            }
        }
        let step_px = self.spec.step as f32 * self.px_per_tick;
        if step_px < MIN_LINE_PX {
            return;
        }
        for t in self.ticks(self.spec.step, w) {
            let color = match self.spec.line(t) {
                // Zoomed far out every line is a bar: only every fourth one is strong.
                Line::Bar if bar_px >= 24. || t.rem_euclid(self.spec.bar * 4) == 0 => pal.grid_bar,
                Line::Bar | Line::Beat => pal.grid_beat,
                Line::Sub => pal.grid_sub,
            };
            window.paint_quad(paint::fill_rect(at(o, self.x(t), 0., 1., h), color));
        }
    }

    /// Ruler ticks and labels: bar numbers that thin out when zoomed out,
    /// `3.2` beats and `3.2.3` sixteenths when zoomed in. `h` is the
    /// ruler's height; labels sit `label_y` from its top.
    pub fn paint_ruler(&self, window: &mut Window, cx: &mut App, o: Point<Pixels>, w: f32, h: f32, label_y: f32) {
        let pal = p();
        let step_px = self.spec.step as f32 * self.px_per_tick;
        if step_px >= 6. && self.spec.step < self.label_step {
            for t in self.ticks(self.spec.step, w).filter(|t| *t >= 0 && t.rem_euclid(self.label_step) != 0) {
                let len = if self.spec.line(t) == Line::Sub { 3. } else { 5. };
                window.paint_quad(paint::fill_rect(at(o, self.x(t), h - len - 1., 1., len), content(0.16)));
            }
        }
        for t in self.ticks(self.label_step, w).filter(|t| *t >= 0) {
            let x = self.x(t);
            let bar = self.spec.line(t) == Line::Bar;
            window.paint_quad(paint::fill_rect(at(o, x, h - 10., 1., 10.), content(if bar { 0.3 } else { 0.2 })));
            let label = crate::nav::position_label(t, self.sig);
            paint::text(window, cx, &label, o.x + px(x + 4.), o.y + px(label_y), TEXT_XS, if bar { pal.muted } else { pal.faint }, FontWeight::MEDIUM, TextAlign::Left);
        }
    }

    /// The time selection as a tinted band over its lanes.
    pub fn paint_time_selection(&self, window: &mut Window, o: Point<Pixels>) {
        let Some(band) = &self.time else { return };
        for (top, h) in &band.lanes {
            window.paint_quad(paint::fill_rect(at(o, band.left, *top, band.right - band.left, *h), accent_alpha(0.16)));
            window.paint_quad(paint::fill_rect(at(o, band.left, *top, 1., *h), accent_alpha(0.7)));
            window.paint_quad(paint::fill_rect(at(o, band.right - 1., *top, 1., *h), accent_alpha(0.7)));
        }
    }

    /// The time selection's extent on the ruler.
    pub fn paint_ruler_selection(&self, window: &mut Window, o: Point<Pixels>, h: f32) {
        if let Some(band) = &self.time {
            window.paint_quad(paint::fill_rect(at(o, band.left, h - 4., band.right - band.left, 3.), accent_alpha(0.55)));
        }
    }

    /// The insert marker: a thin line through the canvas.
    pub fn paint_marker(&self, window: &mut Window, o: Point<Pixels>, w: f32, h: f32) {
        if self.marker >= 0. && self.marker <= w {
            window.paint_quad(paint::fill_rect(at(o, self.marker, 0., 1., h), content(0.7)));
        }
    }

    /// The insert marker on the ruler: a pole with a small flag at its top
    /// (above the labels, so it never covers a bar number).
    pub fn paint_marker_flag(&self, window: &mut Window, o: Point<Pixels>, w: f32, h: f32) {
        if self.marker < 0. || self.marker > w {
            return;
        }
        let x = self.marker;
        window.paint_quad(paint::fill_rect(at(o, x, 1., 1., h - 1.), content(0.7)));
        paint::polygon(window, &[point(o.x + px(x), o.y + px(1.)), point(o.x + px(x + 7.), o.y + px(4.5)), point(o.x + px(x), o.y + px(8.))], content(0.7));
    }
}

/// Where a looping clip starts over, in pixels from its left edge: the
/// first pass runs from the clip's offset to the loop's end, the rest are
/// whole loops. Empty for a clip that does not loop.
pub fn loop_repeats(clip: &Clip, px_per_tick: f32) -> Vec<f32> {
    let Some(l) = clip.looping.filter(|l| l.end > l.start) else { return Vec::new() };
    let cycle = l.end - l.start;
    // A repeat narrower than this is not worth a notch.
    if cycle as f32 * px_per_tick < 4. {
        return Vec::new();
    }
    let mut at = if clip.offset < l.end { l.end - clip.offset } else { cycle - (clip.offset - l.start) % cycle };
    let mut out = Vec::new();
    while at < clip.length {
        out.push(at as f32 * px_per_tick);
        at += cycle;
    }
    out
}

/// A faint line with a notch at its top at each repeat of a looping clip.
/// `x` is the clip's left edge, `y`/`h` the body under its title.
pub fn paint_repeats(window: &mut Window, o: Point<Pixels>, x: f32, y: f32, h: f32, repeats: &[f32], color: Hsla) {
    for r in repeats {
        let rx = x + r;
        window.paint_quad(paint::fill_rect(at(o, rx, y, 1., h), with_alpha(color, 0.28)));
        paint::polygon(window, &[point(o.x + px(rx - 3.), o.y + px(y)), point(o.x + px(rx + 4.), o.y + px(y)), point(o.x + px(rx + 0.5), o.y + px(y + 4.))], with_alpha(color, 0.6));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_looping_clip_repeats_where_its_loop_ends() {
        let mut clip = Clip::new("Loop", Color::palette(0), SourceId(1), 10 * PPQ);
        assert!(loop_repeats(&clip, 0.1).is_empty(), "no loop, no notch");
        clip.looping = Some(LoopRegion { start: 0, end: 4 * PPQ });
        let ticks = |c: &Clip| loop_repeats(c, 1.).into_iter().map(|x| x as Tick).collect::<Vec<_>>();
        assert_eq!(ticks(&clip), [4 * PPQ, 8 * PPQ]);
        // Starting one beat in, the first pass is three beats long.
        clip.offset = PPQ;
        assert_eq!(ticks(&clip), [3 * PPQ, 7 * PPQ]);
        // A loop a pixel wide draws nothing.
        assert!(loop_repeats(&clip, 0.0005).is_empty());
    }
}
