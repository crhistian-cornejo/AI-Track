//! Low-level painting for the dense views (clips, notes, grids, meters,
//! knobs): rounded rects, arcs and single-line text drawn straight into a
//! `canvas`, so a timeline with thousands of notes is a handful of draw
//! calls rather than thousands of elements.

use crate::theme;
use gpui::{
    App, Bounds, Corners, FontWeight, Hsla, PaintQuad, PathBuilder, Pixels, Point, SharedString, TextAlign, TextRun,
    Window, fill, point, px, quad, size,
};

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(w.max(0.)), px(h.max(0.))))
}

/// Offset a local rect by the canvas origin.
pub fn at(origin: Point<Pixels>, x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(origin.x + px(x), origin.y + px(y)), size(px(w.max(0.)), px(h.max(0.))))
}

pub fn rrect(bounds: Bounds<Pixels>, radius: f32, color: Hsla) -> PaintQuad {
    quad(bounds, Corners::all(px(radius)), color, px(0.), gpui::transparent_black(), Default::default())
}

pub fn rrect_border(bounds: Bounds<Pixels>, radius: f32, bg: Hsla, border: Hsla, width: f32) -> PaintQuad {
    quad(bounds, Corners::all(px(radius)), bg, px(width), border, Default::default())
}

pub fn fill_rect(bounds: Bounds<Pixels>, color: Hsla) -> PaintQuad {
    fill(bounds, color)
}

/// A stroked polyline.
pub fn polyline(window: &mut Window, points: &[Point<Pixels>], width: f32, color: Hsla) {
    if points.len() < 2 {
        return;
    }
    let mut b = PathBuilder::stroke(px(width));
    b.move_to(points[0]);
    for p in &points[1..] {
        b.line_to(*p);
    }
    if let Ok(path) = b.build() {
        window.paint_path(path, color);
    }
}

/// A filled polygon.
pub fn polygon(window: &mut Window, points: &[Point<Pixels>], color: Hsla) {
    if points.len() < 3 {
        return;
    }
    let mut b = PathBuilder::fill();
    b.add_polygon(points, true);
    if let Ok(path) = b.build() {
        window.paint_path(path, color);
    }
}

/// An arc stroke from `a0` to `a1` (radians, 0 = right, clockwise on screen).
pub fn arc(window: &mut Window, center: Point<Pixels>, radius: f32, a0: f32, a1: f32, width: f32, color: Hsla) {
    let span = a1 - a0;
    if span.abs() < 1e-3 {
        return;
    }
    let steps = ((span.abs() * radius / 2.).ceil() as usize).clamp(6, 96);
    let pts: Vec<Point<Pixels>> = (0..=steps)
        .map(|i| {
            let a = a0 + span * i as f32 / steps as f32;
            point(center.x + px(radius * a.cos()), center.y + px(radius * a.sin()))
        })
        .collect();
    polyline(window, &pts, width, color);
}

/// Single-line text. `align` positions it relative to `x` (left, centre or right edge).
#[allow(clippy::too_many_arguments)]
pub fn text(
    window: &mut Window,
    cx: &mut App,
    s: &str,
    x: Pixels,
    y: Pixels,
    font_size: f32,
    color: Hsla,
    weight: FontWeight,
    align: TextAlign,
) -> f32 {
    if s.is_empty() {
        return 0.;
    }
    let mut font = gpui::font(theme::FONT_UI);
    font.weight = weight;
    let run = TextRun { len: s.len(), font, color, background_color: None, underline: None, strikethrough: None };
    let line = window.text_system().shape_line(SharedString::from(s.to_owned()), px(font_size), &[run], None);
    let w = f32::from(line.width);
    let left = match align {
        TextAlign::Left => x,
        TextAlign::Center => x - px(w / 2.),
        TextAlign::Right => x - px(w),
    };
    let lh = px((font_size * 1.3).round());
    let _ = line.paint(point(left, y), lh, TextAlign::Left, None, window, cx);
    w
}

/// Text clipped to `max_w` with an ellipsis (cheap: measures by chars).
pub fn fit(s: &str, max_w: f32, font_size: f32) -> String {
    let per = font_size * 0.56;
    let max_chars = (max_w / per).floor() as usize;
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    if max_chars <= 1 {
        return String::new();
    }
    let mut out: String = s.chars().take(max_chars - 1).collect();
    out.push('…');
    out
}
