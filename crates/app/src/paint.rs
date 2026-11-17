//! Low-level painting for the dense views (clips, notes, grids, meters,
//! knobs): rounded rects, arcs and single-line text drawn straight into a
//! `canvas`, so a timeline with thousands of notes is a handful of draw
//! calls rather than thousands of elements.

use crate::theme;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use gpui::{
    App, Bounds, Corners, FontWeight, Hsla, PaintQuad, Path, PathBuilder, Pixels, Point, SharedString, TextAlign, TextRun,
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

/// Tessellated shapes kept for the frames that draw the same shape again: a
/// response curve or a filled area only changes when its parameters do, and
/// tessellating (lyon) costs far more than painting what it produced. A shape is
/// remembered by its points relative to the first one (to 1/32 px), so it also
/// hits when the shape moves.
mod shapes {
    use super::{Path, Pixels, Point, moved};
    use gpui::{PathBuilder, px};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::rc::Rc;

    /// Longest shape worth remembering (a 4 096-point scope is not).
    const MAX_POINTS: usize = 600;
    /// Vertices kept in all (about 12 MB), and how many calls an entry may sit unused.
    const MAX_VERTICES: usize = 400_000;
    const STALE_AFTER: u64 = 4_000;

    struct Entry {
        pts: Vec<(i32, i32)>,
        path: Option<Rc<Path<Pixels>>>,
        touched: u64,
    }

    #[derive(Default)]
    struct Cache {
        map: HashMap<u64, Entry>,
        vertices: usize,
        calls: u64,
    }

    thread_local! {
        static CACHE: RefCell<Cache> = RefCell::default();
    }

    // Hits and misses since the last `take_stats` (tests only).
    #[cfg(test)]
    thread_local! {
        static STATS: RefCell<(u32, u32)> = const { RefCell::new((0, 0)) };
    }

    #[cfg(test)]
    pub fn take_stats() -> (u32, u32) {
        STATS.with_borrow_mut(std::mem::take)
    }

    #[cfg(test)]
    fn count(hit: bool) {
        STATS.with_borrow_mut(|s| if hit { s.0 += 1 } else { s.1 += 1 });
    }

    #[cfg(not(test))]
    fn count(_: bool) {}

    fn quantised(points: &[Point<Pixels>]) -> Vec<(i32, i32)> {
        let o = points[0];
        points.iter().map(|p| (((f32::from(p.x - o.x)) * 32.).round() as i32, ((f32::from(p.y - o.y)) * 32.).round() as i32)).collect()
    }

    /// The path of `points` drawn as a stroke of `width` (`fill` = false) or as a filled
    /// polygon, moved to where `points` are.
    pub fn path(points: &[Point<Pixels>], width: f32, fill: bool) -> Option<Path<Pixels>> {
        if points.len() > MAX_POINTS {
            return build(points, width, fill);
        }
        let pts = quantised(points);
        let mut h = DefaultHasher::new();
        (fill, (width * 16.).round() as i32).hash(&mut h);
        pts.hash(&mut h);
        let key = h.finish();
        CACHE.with_borrow_mut(|c| {
            c.calls += 1;
            let calls = c.calls;
            if let Some(e) = c.map.get_mut(&key)
                && e.pts == pts
            {
                e.touched = calls;
                count(true);
                return e.path.as_ref().map(|p| moved(p, points[0]));
            }
            count(false);
            if c.vertices > MAX_VERTICES {
                c.map.retain(|_, e| e.touched + STALE_AFTER > calls);
                c.vertices = c.map.values().map(|e| e.path.as_ref().map_or(0, |p| p.vertices.len())).sum();
                if c.vertices > MAX_VERTICES {
                    c.map.clear();
                    c.vertices = 0;
                }
            }
            // Built at the origin from the quantised points, so a hit and a miss draw alike.
            let local: Vec<Point<Pixels>> = pts.iter().map(|(x, y)| Point { x: px(*x as f32 / 32.), y: px(*y as f32 / 32.) }).collect();
            let path = build(&local, width, fill).map(Rc::new);
            c.vertices += path.as_ref().map_or(0, |p| p.vertices.len());
            let out = path.as_ref().map(|p| moved(p, points[0]));
            c.map.insert(key, Entry { pts, path, touched: calls });
            out
        })
    }

    fn build(points: &[Point<Pixels>], width: f32, fill: bool) -> Option<Path<Pixels>> {
        let mut b = if fill { PathBuilder::fill() } else { PathBuilder::stroke(px(width)) };
        if fill {
            b.add_polygon(points, true);
        } else {
            b.move_to(points[0]);
            for p in &points[1..] {
                b.line_to(*p);
            }
        }
        b.build().ok()
    }
}

/// A stroked polyline.
pub fn polyline(window: &mut Window, points: &[Point<Pixels>], width: f32, color: Hsla) {
    if points.len() < 2 {
        return;
    }
    if let Some(path) = shapes::path(points, width, false) {
        window.paint_path(path, color);
    }
}

/// A filled polygon.
pub fn polygon(window: &mut Window, points: &[Point<Pixels>], color: Hsla) {
    if points.len() < 3 {
        return;
    }
    if let Some(path) = shapes::path(points, 0., true) {
        window.paint_path(path, color);
    }
}

/// The tessellated stroke of an arc around the origin, kept by shape: a knob's track
/// never changes, and tessellating (lyon) costs far more than painting.
fn arc_path(radius: f32, a0: f32, a1: f32, width: f32) -> Option<Rc<Path<Pixels>>> {
    type Key = (i32, i32, i32, i32);
    thread_local! {
        static ARCS: RefCell<HashMap<Key, Option<Rc<Path<Pixels>>>>> = RefCell::new(HashMap::new());
    }
    let key = ((radius * 16.).round() as i32, (a0 * 1024.).round() as i32, (a1 * 1024.).round() as i32, (width * 16.).round() as i32);
    ARCS.with_borrow_mut(|cache| {
        if cache.len() > 768 {
            // A value arc that sweeps (automation, a drag) makes a new shape per frame: start over.
            cache.clear();
        }
        cache
            .entry(key)
            .or_insert_with(|| {
                let span = a1 - a0;
                // A chord every 3 px keeps the curve within 0.1 px of the circle.
                let steps = ((span.abs() * radius / 3.).ceil() as usize).clamp(6, 96);
                let mut b = PathBuilder::stroke(px(width));
                for i in 0..=steps {
                    let a = a0 + span * i as f32 / steps as f32;
                    let pt = point(px(radius * a.cos()), px(radius * a.sin()));
                    if i == 0 {
                        b.move_to(pt);
                    } else {
                        b.line_to(pt);
                    }
                }
                b.build().ok().map(Rc::new)
            })
            .clone()
    })
}

/// `path` moved by `by`.
fn moved(path: &Path<Pixels>, by: Point<Pixels>) -> Path<Pixels> {
    let mut q = path.clone();
    for v in &mut q.vertices {
        v.xy_position += by;
    }
    q.bounds.origin += by;
    q
}

/// An arc stroke from `a0` to `a1` (radians, 0 = right, clockwise on screen).
pub fn arc(window: &mut Window, center: Point<Pixels>, radius: f32, a0: f32, a1: f32, width: f32, color: Hsla) {
    if (a1 - a0).abs() < 1e-3 {
        return;
    }
    if let Some(path) = arc_path(radius, a0, a1, width) {
        window.paint_path(moved(&path, center), color);
    }
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
    let font_size = theme::fs(font_size);
    let mut font = gpui::font(theme::font_ui());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_arc_moved_to_a_centre_is_the_arc_built_there() {
        let (r, a0, a1, w) = (13., 2.1, 6.9, 3.);
        let cached = arc_path(r, a0, a1, w).expect("arc");
        let centre = point(px(40.), px(25.));
        let moved = moved(&cached, centre);
        // The same arc tessellated straight at that centre.
        let mut b = PathBuilder::stroke(px(w));
        let steps = ((a1 - a0) * r / 3.).ceil() as usize;
        for i in 0..=steps {
            let a = a0 + (a1 - a0) * i as f32 / steps as f32;
            let pt = point(centre.x + px(r * a.cos()), centre.y + px(r * a.sin()));
            if i == 0 { b.move_to(pt) } else { b.line_to(pt) }
        }
        let direct = b.build().unwrap();
        assert_eq!(moved.vertices.len(), direct.vertices.len());
        for (m, d) in moved.vertices.iter().zip(&direct.vertices) {
            assert!((f32::from(m.xy_position.x) - f32::from(d.xy_position.x)).abs() < 1e-2);
            assert!((f32::from(m.xy_position.y) - f32::from(d.xy_position.y)).abs() < 1e-2);
        }
    }

    #[test]
    fn the_same_arc_is_tessellated_once() {
        let a = arc_path(9., 2.1, 5., 3.).unwrap();
        let b = arc_path(9., 2.1, 5., 3.).unwrap();
        assert!(Rc::ptr_eq(&a, &b));
    }

    #[test]
    fn a_curve_drawn_twice_is_tessellated_once_even_when_it_moves() {
        let curve = |dx: f32, bump: f32| -> Vec<Point<Pixels>> { (0..80).map(|i| point(px(dx + i as f32 * 3.), px(40. + (i as f32 * 0.2).sin() * 12. + bump))).collect() };
        shapes::take_stats();
        let a = shapes::path(&curve(10., 0.), 1.5, false).unwrap();
        let b = shapes::path(&curve(10., 0.), 1.5, false).unwrap();
        let c = shapes::path(&curve(70., 5.), 1.5, false).unwrap();
        assert_eq!(shapes::take_stats(), (2, 1), "the second and the moved copy hit");
        assert_eq!(a.vertices.len(), b.vertices.len());
        // The moved copy is the same shape, shifted by the new origin.
        let (dx, dy) = (f32::from(c.vertices[0].xy_position.x - a.vertices[0].xy_position.x), f32::from(c.vertices[0].xy_position.y - a.vertices[0].xy_position.y));
        assert!((dx - 60.).abs() < 1e-2 && (dy - 5.).abs() < 1e-2, "{dx} {dy}");
        // A curve that changed is a new shape.
        let _ = shapes::path(&curve(10., 0.).into_iter().enumerate().map(|(i, p)| if i == 40 { point(p.x, p.y + px(3.)) } else { p }).collect::<Vec<_>>(), 1.5, false);
        assert_eq!(shapes::take_stats(), (0, 1));
    }

    #[test]
    fn a_polygon_and_a_line_through_the_same_points_are_different_shapes() {
        let pts: Vec<Point<Pixels>> = [(0., 0.), (20., 0.), (20., 20.), (0., 20.)].iter().map(|(x, y)| point(px(*x), px(*y))).collect();
        shapes::take_stats();
        let fill = shapes::path(&pts, 0., true).unwrap();
        let line = shapes::path(&pts, 2., false).unwrap();
        assert_eq!(shapes::take_stats(), (0, 2));
        assert_ne!(fill.vertices.len(), line.vertices.len());
    }
}
