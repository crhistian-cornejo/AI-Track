//! Painting of an audio clip in its lane: the waveform as it will sound, the
//! fades with their handles, and the loop glyph of the title. Positions come
//! from `geom`; everything is in pixels from the canvas' origin `o`.

use super::geom::{self, ClipBox, HANDLE, HEAD, SPLIT_MIN_H, WaveMap, Zone};
use crate::icons::Icon;
use crate::paint::{self, at};
use crate::theme::*;
use crate::waveform::Peaks;
use aitrack_model::{Clip, curve_shape};
use gpui::{App, Hsla, Pixels, Point, TransformationMatrix, Window, point, px};
use std::cell::RefCell;
use std::sync::Arc;

/// Side of the loop glyph in a clip's title.
pub const GLYPH: f32 = 10.;

thread_local! {
    /// The pixel columns of the clip being painted: one buffer for every
    /// clip of every frame.
    static COLUMNS: RefCell<Vec<(f32, f32)>> = const { RefCell::new(Vec::new()) };
}

/// The decoded file of an audio clip, ready to draw.
#[derive(Clone)]
pub struct Wave {
    pub peaks: Arc<Peaks>,
    /// Frames of `peaks` per source tick.
    pub frames_per_tick: f64,
    /// Transpose and detune as a speed (Re-Pitch), and whether it plays reversed.
    pub ratio: f64,
    pub reverse: bool,
}

/// What shapes the level of `c`'s waveform (gain, fades, volume envelope,
/// offset, loop) without its name or its other envelopes, so gathering a
/// lane's clips does not allocate for a clip that has no volume envelope.
pub fn shape_of(c: &Clip) -> Clip {
    Clip {
        name: String::new(),
        color: c.color,
        source: c.source,
        offset: c.offset,
        length: c.length,
        looping: c.looping,
        gain_db: c.gain_db,
        envelopes: c.volume_envelope().cloned().into_iter().collect(),
        launch: c.launch.clone(),
        muted: c.muted,
        scale: c.scale,
        fade_in: c.fade_in,
        fade_out: c.fade_out,
    }
}

/// The waveform of `clip` inside `bx`, between the canvas' left edge and
/// `canvas_w`: one min/max column per pixel on screen, scaled by the clip
/// gain, the fades and the volume envelope. Without `wave` (the file is
/// still being read, or cannot be) only a quiet centre line.
pub fn wave(window: &mut Window, o: Point<Pixels>, bx: &ClipBox, clip: &Clip, wave: Option<&Wave>, canvas_w: f32, color: Hsla) {
    let (top, h) = bx.body();
    // Whole pixels, so the columns do not shimmer against the pixel grid.
    let (xa, xb) = ((bx.x + 1.).max(0.).floor(), (bx.x + bx.w - 1.).min(canvas_w).ceil());
    if xb <= xa || h < 4. {
        return;
    }
    let Some(wave) = wave else {
        window.paint_quad(paint::fill_rect(at(o, xa, top + h / 2., xb - xa, 1.), with_alpha(color, 0.35)));
        return;
    };
    let split = wave.peaks.channels() >= 2 && h >= SPLIT_MIN_H;
    let rows: &[(Option<usize>, f32, f32)] =
        if split { &[(Some(0), top + h * 0.25, h * 0.25 - 1.), (Some(1), top + h * 0.75, h * 0.25 - 1.)] } else { &[(None, top + h * 0.5, h * 0.5 - 1.)] };
    let map = WaveMap::of(clip, wave.frames_per_tick, f64::from(bx.px_per_tick))
        .pitched(wave.ratio, wave.reverse.then(|| wave.peaks.frames() as f64));
    let t0 = f64::from(xa - bx.x) * map.ticks_per_px;
    let count = (xb - xa) as usize;
    let outline = style().waveform == WaveformStyle::Outline;
    COLUMNS.with_borrow_mut(|cols| {
        for &(channel, mid, half) in rows {
            geom::columns(&wave.peaks, channel, clip, &map, t0, count, cols);
            let mut prev: Option<(f32, f32)> = None;
            for (i, (lo, hi)) in cols.iter().enumerate() {
                let x = xa + i as f32;
                let (y_hi, y_lo) = (mid - hi * half, mid - lo * half);
                if outline {
                    // Only the two edges, each joined to the column before.
                    let (p_hi, p_lo) = prev.unwrap_or((y_hi, y_lo));
                    window.paint_quad(paint::fill_rect(at(o, x, y_hi.min(p_hi), 1., (y_hi - p_hi).abs().max(1.)), color));
                    window.paint_quad(paint::fill_rect(at(o, x, y_lo.min(p_lo), 1., (y_lo - p_lo).abs().max(1.)), color));
                    prev = Some((y_hi, y_lo));
                } else {
                    window.paint_quad(paint::fill_rect(at(o, x, y_hi, 1., (y_lo - y_hi).max(1.)), color));
                }
            }
        }
    });
}

/// Both fades of an audio clip: each one's slope as a line, with the corner
/// it takes away darkened.
pub fn fades(window: &mut Window, o: Point<Pixels>, bx: &ClipBox, canvas_w: f32, hot: Option<Zone>, line: Hsla) {
    if !bx.audio || bx.body().1 < 4. {
        return;
    }
    for fade_in in [true, false] {
        let fade = if fade_in { bx.fade_in } else { bx.fade_out };
        let (x0, x1) = if fade_in { (bx.x, bx.fade_end(true)) } else { (bx.fade_end(false), bx.x + bx.w) };
        // Only the part on screen, so a long fade stays smooth zoomed in.
        let (from, to) = (x0.max(-8.), x1.min(canvas_w + 8.));
        if fade.length <= 0 || x1 - x0 < 1. || to <= from {
            continue;
        }
        let held = matches!((hot, fade_in), (Some(Zone::FadeIn | Zone::FadeInCurve), true) | (Some(Zone::FadeOut | Zone::FadeOutCurve), false));
        let n = (((to - from) / 4.).ceil() as usize).clamp(2, 256);
        let mut pts: Vec<Point<Pixels>> = (0..=n)
            .map(|i| {
                let x = from + (to - from) * i as f32 / n as f32;
                let along = (x - x0) / (x1 - x0);
                point(o.x + px(x), o.y + px(bx.gain_y(curve_shape(if fade_in { along } else { 1. - along }, fade.curve))))
            })
            .collect();
        paint::polyline(window, &pts, 1.5, if held { accent() } else { line });
        // The corner above the slope.
        let top = o.y + px(bx.gain_y(1.));
        pts.push(point(o.x + px(to), top));
        pts.push(point(o.x + px(from), top));
        paint::polygon(window, &pts, shadow(0.32));
    }
}

/// The fade handles of a clip that is hovered, selected or being dragged:
/// a square in each top corner, and a dot on each slope long enough to bend.
pub fn handles(window: &mut Window, o: Point<Pixels>, bx: &ClipBox, hot: Option<Zone>) {
    if !bx.has_handles() {
        return;
    }
    let pal = p();
    for (fade_in, corner, slope) in [(true, Zone::FadeIn, Zone::FadeInCurve), (false, Zone::FadeOut, Zone::FadeOutCurve)] {
        if let Some((x, y)) = bx.slope(fade_in) {
            let held = hot == Some(slope);
            let side = if held { 9. } else { 7. };
            window.paint_quad(paint::rrect_border(at(o, x - side / 2., y - side / 2., side, side), side / 2., pal.well, if held { pal.accent } else { pal.text }, 1.5));
        }
        let (x, y) = bx.handle(fade_in);
        let held = hot == Some(corner);
        let side = if held { HANDLE + 2. } else { HANDLE };
        window.paint_quad(paint::rrect_border(at(o, x - side / 2., y - side / 2., side, side), 2., if held { pal.accent } else { pal.text }, pal.well, 1.));
    }
}

/// The loop glyph at the right end of a looping clip's title. Returns the
/// width it takes from the name.
pub fn loop_glyph(window: &mut Window, cx: &mut App, o: Point<Pixels>, bx: &ClipBox, color: Hsla) -> f32 {
    if bx.w < 44. {
        return 0.;
    }
    let bounds = at(o, bx.x + bx.w - GLYPH - 4., bx.y + (HEAD - GLYPH) / 2., GLYPH, GLYPH);
    // A missing asset only costs the glyph.
    let _ = window.paint_svg(bounds, Icon::Loop.path().into(), None, TransformationMatrix::unit(), color, cx);
    GLYPH + 4.
}
