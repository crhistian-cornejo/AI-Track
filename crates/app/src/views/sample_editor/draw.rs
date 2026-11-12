//! Painting of the sample editor's canvas: ruler, grid, waveform, fades,
//! volume envelope, markers, loop brace, cursor, selection and playhead.
//! Everything comes in through a [`Scene`]; positions come from `geom`.

use super::geom::{self, Axis, GAIN_TOP, HANDLE_Y, Hit, Layout, RULER, TimeBase};
use crate::grid::{GridSpec, Line};
use crate::paint::{self, at};
use crate::theme::*;
use crate::waveform::{self, Peaks};
use aitrack_model::{Clip, PPQ, Tick, curve_shape};
use gpui::{App, Bounds, ContentMask, FontWeight, Hsla, Pixels, Point, TextAlign, Window, point, px};
use std::sync::Arc;

/// Two channels are drawn apart once each lane is at least this tall.
const LANE_MIN: f32 = 48.;
/// From this many pixels per sample on, single samples get a dot.
const DOT_PX: f64 = 6.;
/// Ruler labels are at least this far apart.
const LABEL_PX: f64 = 56.;

/// What the canvas shows this frame.
pub struct Scene {
    pub clip: Clip,
    pub axis: Axis,
    /// The file's time at the sample rate of `peaks`.
    pub base: TimeBase,
    /// Axis position of song tick 0.
    pub origin: Tick,
    pub grid: GridSpec,
    pub peaks: Option<Arc<Peaks>>,
    /// Why there is no waveform (still decoding, or unreadable).
    pub note: Option<String>,
    pub cursor: Option<Tick>,
    pub selection: Option<(Tick, Tick)>,
    /// The transport on this axis, and whether it runs.
    pub playhead: Option<(Tick, bool)>,
    /// The target under the pointer or being dragged.
    pub hot: Option<Hit>,
    /// The breakpoint clicked last.
    pub picked: Option<usize>,
    /// What the dragged handle reads.
    pub readout: Option<String>,
    /// The clip's colour.
    pub color: Hsla,
    /// The sample plays reversed: the file is drawn back to front.
    pub reverse: bool,
}

struct Pen<'a> {
    s: &'a Scene,
    l: Layout,
    o: Point<Pixels>,
}

impl Pen<'_> {
    fn pt(&self, x: f32, y: f32) -> Point<Pixels> {
        point(self.o.x + px(x), self.o.y + px(y))
    }

    /// The part of the canvas under the ruler.
    fn wave(&self) -> (f32, f32) {
        (RULER, (self.l.h - RULER).max(0.))
    }
}

pub fn paint(s: &Scene, b: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    let l = Layout { axis: s.axis, w: f32::from(b.size.width), h: f32::from(b.size.height) };
    if l.w <= 0. || l.h <= RULER {
        return;
    }
    let pen = Pen { s, l, o: b.origin };
    window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
        let pal = p();
        let c = &s.clip;
        let (top, wave_h) = pen.wave();
        let (xs, xe) = (l.x(c.offset), l.x(geom::end_u(c)));
        window.paint_quad(paint::fill_rect(b, pal.well));
        window.paint_quad(paint::fill_rect(at(pen.o, 0., 0., l.w, RULER), pal.surface));
        // The part the clip plays, lifted a little.
        let (ps, pe) = (xs.clamp(0., l.w), xe.clamp(0., l.w));
        if pe > ps {
            window.paint_quad(paint::fill_rect(at(pen.o, ps, top, pe - ps, wave_h), with_alpha(s.color, 0.07)));
        }
        grid(&pen, window, cx);
        match &s.peaks {
            Some(peaks) => waveform(&pen, peaks, window),
            // No peaks: the flat line of a file that is silent, loading or unreadable.
            None => window.paint_quad(paint::fill_rect(at(pen.o, 0., top + wave_h / 2., l.w, 1.), content(0.14))),
        }
        // Everything the clip does not play is dimmed.
        let dim = with_alpha(pal.well, 0.62);
        if ps > 0. {
            window.paint_quad(paint::fill_rect(at(pen.o, 0., top, ps, wave_h), dim));
        }
        if pe < l.w {
            window.paint_quad(paint::fill_rect(at(pen.o, pe, top, l.w - pe, wave_h), dim));
        }
        repeats(&pen, window);
        if let Some((a, b)) = s.selection {
            let (xa, xb) = (l.x(a).clamp(-2., l.w + 2.), l.x(b).clamp(-2., l.w + 2.));
            window.paint_quad(paint::fill_rect(at(pen.o, xa, top, xb - xa, wave_h), accent_alpha(0.14)));
            window.paint_quad(paint::fill_rect(at(pen.o, xa, top, 1., wave_h), accent_alpha(0.6)));
            window.paint_quad(paint::fill_rect(at(pen.o, xb - 1., top, 1., wave_h), accent_alpha(0.6)));
        }
        fade(&pen, true, window);
        fade(&pen, false, window);
        envelope(&pen, window);
        brace(&pen, window);
        markers(&pen, window);
        if let Some(u) = s.cursor {
            let x = l.x(u);
            if x >= -1. && x <= l.w + 1. {
                window.paint_quad(paint::fill_rect(at(pen.o, x, RULER - 6., 1., l.h - RULER + 6.), content(0.75)));
                paint::polygon(window, &[pen.pt(x - 4., RULER - 6.), pen.pt(x + 5., RULER - 6.), pen.pt(x + 0.5, RULER)], content(0.75));
            }
        }
        if let Some((u, running)) = s.playhead {
            let x = l.x(u);
            if x >= 0. && x <= l.w {
                window.paint_quad(paint::fill_rect(at(pen.o, x, 0., 1., l.h), if running { accent() } else { accent_alpha(0.45) }));
            }
        }
        window.paint_quad(paint::fill_rect(at(pen.o, 0., RULER - 1., l.w, 1.), border()));
        if let Some(text) = &s.note {
            let text = paint::fit(text, l.w - 2. * SP3, TEXT_SM);
            paint::text(window, cx, &text, pen.o.x + px(SP3), pen.o.y + px(l.h - 20.), TEXT_SM, pal.faint, FontWeight::NORMAL, TextAlign::Left);
        }
        if let (Some(text), Some(hit)) = (&s.readout, s.hot) {
            readout(&pen, text, hit, window, cx);
        }
    });
}

/// Grid lines in the song's time, with bar numbers on the ruler.
fn grid(pen: &Pen, window: &mut Window, cx: &mut App) {
    let pal = p();
    let (s, l) = (pen.s, &pen.l);
    let (top, wave_h) = pen.wave();
    let ppt = l.axis.px_per_tick();
    // Zoomed all the way out even the coarsest step gets crowded: widen it.
    let mut step = s.grid.step.max(1);
    while (step as f64 * ppt) < 6. {
        step *= 2;
    }
    let (bar, beat, sixteenth) = (s.grid.bar, s.grid.beat, PPQ / 4);
    // What gets a label: the finest of step, sixteenth, beat, bar, 2 bars… that has room.
    let wide = |unit: Tick| unit as f64 * ppt >= LABEL_PX;
    let mut unit = bar;
    if wide(beat) {
        unit = beat;
        if step <= sixteenth && sixteenth % step == 0 && wide(sixteenth) {
            unit = sixteenth;
            if step < sixteenth && wide(step) {
                unit = step;
            }
        }
    } else {
        while !wide(unit) {
            unit *= 2;
        }
    }
    let first = ((l.axis.start - s.origin as f64) / step as f64).floor() as i64;
    let count = (f64::from(l.w) / (step as f64 * ppt)).ceil() as i64 + 1;
    for i in 0..=count {
        let song = (first + i) * step;
        let x = l.x(s.origin + song);
        if x < -1. || x > l.w {
            continue;
        }
        let (line, tick_h, ruler) = match s.grid.line(song) {
            Line::Bar => (pal.grid_bar, RULER - 8., content(0.3)),
            Line::Beat => (pal.grid_beat, 6., content(0.18)),
            Line::Sub => (pal.grid_sub, 3., content(0.12)),
        };
        window.paint_quad(paint::fill_rect(at(pen.o, x, top, 1., wave_h), line));
        window.paint_quad(paint::fill_rect(at(pen.o, x, RULER - tick_h, 1., tick_h), ruler));
        if song.rem_euclid(unit) == 0 {
            let in_bar = song.rem_euclid(bar);
            let (b, q, x16, rest) = (song.div_euclid(bar) + 1, in_bar / beat + 1, (in_bar % beat) / sixteenth + 1, (in_bar % beat) % sixteenth);
            let text = if unit >= bar {
                format!("{b}")
            } else if unit >= beat {
                format!("{b}.{q}")
            } else if unit >= sixteenth {
                format!("{b}.{q}.{x16}")
            } else {
                format!("{b}.{q}.{x16}+{rest}")
            };
            paint::text(window, cx, &text, pen.o.x + px(x + 4.), pen.o.y + px(3.), TEXT_XS, pal.muted, FontWeight::MEDIUM, TextAlign::Left);
        }
    }
}

/// The waveform as it will sound: scaled by the clip gain, the fades and
/// the volume envelope where the clip plays.
fn waveform(pen: &Pen, peaks: &Peaks, window: &mut Window) {
    let pal = p();
    let (s, l) = (pen.s, &pen.l);
    let c = &s.clip;
    let (top, wave_h) = pen.wave();
    let frames_per_tick = s.base.tick_to_frame(1.);
    let frames_per_px = frames_per_tick / l.axis.px_per_tick();
    let clip_gain = geom::db_to_gain(c.gain_db);
    let gain = |u: f64| {
        let t = u - c.offset as f64;
        if t >= 0. && t <= c.length as f64 { c.gain_at(t.round() as Tick) * clip_gain } else { clip_gain }
    };
    // Without a loop there is nothing past the end of the file.
    let file_end = peaks.frames() as f64 / frames_per_tick;
    // Reversed, frame `f` of what plays is frame `n - 1 - f` of the file.
    let n = peaks.frames() as f64;
    let flip = |f: f64| if s.reverse { n - 1. - f } else { f };
    let loops = c.looping.is_some_and(|lp| lp.end > lp.start);
    let color = mix(s.color, pal.waveform, 0.5);
    let outline = style().waveform == WaveformStyle::Outline;
    let split = peaks.channels() >= 2 && wave_h >= 2. * LANE_MIN;
    let lanes: &[(Option<usize>, f32, f32)] = if split {
        &[(Some(0), top + wave_h * 0.25, wave_h * 0.25 - 2.), (Some(1), top + wave_h * 0.75, wave_h * 0.25 - 2.)]
    } else {
        &[(None, top + wave_h * 0.5, wave_h * 0.5 - 3.)]
    };
    if split {
        window.paint_quad(paint::fill_rect(at(pen.o, 0., top + wave_h * 0.5, l.w, 1.), border()));
    }
    for &(channel, mid, half) in lanes {
        window.paint_quad(paint::fill_rect(at(pen.o, 0., mid, l.w, 1.), content(0.07)));
        let y = |v: f32, g: f32| mid - (v * g).clamp(-1., 1.) * half;
        if frames_per_px < 0.5 && peaks.has_samples() {
            // Close enough to see the samples themselves.
            let (first, last) = ((l.axis.tick(0.) * frames_per_tick).floor() as i64 - 1, (l.axis.tick(l.w) * frames_per_tick).ceil() as i64 + 1);
            let dots = 1. / frames_per_px >= DOT_PX;
            let mut run: Vec<Point<Pixels>> = Vec::with_capacity((last - first).max(0) as usize + 1);
            for n in first.max(0)..=last {
                let u = n as f64 / frames_per_tick;
                let frame = (geom::unroll(c.looping, u) * frames_per_tick).round();
                match peaks
                    .sample(channel, flip(frame).max(0.) as u64)
                    .filter(|_| loops || u < file_end)
                {
                    Some(v) => {
                        let at_sample = pen.pt(l.axis.x(u), y(v, gain(u)));
                        if dots {
                            window.paint_quad(paint::rrect(Bounds::new(point(at_sample.x - px(1.5), at_sample.y - px(1.5)), gpui::size(px(3.), px(3.))), 1.5, color));
                        }
                        run.push(at_sample);
                    }
                    None => {
                        paint::polyline(window, &run, 1.25, color);
                        run.clear();
                    }
                }
            }
            paint::polyline(window, &run, 1.25, color);
            continue;
        }
        let mut prev = None;
        let mut prev_edges: Option<(f32, f32)> = None;
        for i in 0..l.w.ceil() as usize {
            let u = l.axis.tick(i as f32);
            if u < 0. || (!loops && u >= file_end) {
                prev = None;
                prev_edges = None;
                continue;
            }
            let a = geom::unroll(c.looping, u) * frames_per_tick;
            let col = if s.reverse {
                peaks.range(channel, n - a - frames_per_px, n - a)
            } else {
                peaks.range(channel, a, a + frames_per_px)
            };
            let (lo, hi) = waveform::join(col, prev);
            prev = col;
            if col.is_none() {
                prev_edges = None;
                continue;
            }
            let g = gain(u + 0.5 / l.axis.px_per_tick());
            let (y_hi, y_lo) = (y(hi, g), y(lo, g));
            if outline {
                // Only the two edges, each joined to the column before.
                let (p_hi, p_lo) = prev_edges.unwrap_or((y_hi, y_lo));
                window.paint_quad(paint::fill_rect(at(pen.o, i as f32, y_hi.min(p_hi), 1., (y_hi - p_hi).abs().max(1.)), color));
                window.paint_quad(paint::fill_rect(at(pen.o, i as f32, y_lo.min(p_lo), 1., (y_lo - p_lo).abs().max(1.)), color));
                prev_edges = Some((y_hi, y_lo));
            } else {
                window.paint_quad(paint::fill_rect(at(pen.o, i as f32, y_hi, 1., (y_lo - y_hi).max(1.)), color));
            }
        }
    }
}

/// Where a looping clip starts over.
fn repeats(pen: &Pen, window: &mut Window) {
    let (s, l) = (pen.s, &pen.l);
    let Some(lp) = s.clip.looping.filter(|lp| lp.end > lp.start) else { return };
    let (top, wave_h) = pen.wave();
    let len = lp.end - lp.start;
    if (len as f64 * l.axis.px_per_tick()) < 6. {
        return;
    }
    let first = (((l.axis.start - lp.end as f64) / len as f64).ceil() as i64).max(0);
    let end = geom::end_u(&s.clip).max(lp.end);
    for k in first.. {
        let u = lp.end + k * len;
        let x = l.x(u);
        if x > l.w || u > end {
            break;
        }
        window.paint_quad(paint::fill_rect(at(pen.o, x, top, 1., wave_h), with_alpha(p().accent2, 0.4)));
    }
}

/// A fade's slope on the gain scale, the part it takes away shaded.
fn fade(pen: &Pen, fade_in: bool, window: &mut Window) {
    let pal = p();
    let (s, l) = (pen.s, &pen.l);
    let c = &s.clip;
    let f = if fade_in { c.fade_in } else { c.fade_out };
    let (target, slope) = if fade_in { (Hit::FadeIn, Hit::FadeInCurve) } else { (Hit::FadeOut, Hit::FadeOutCurve) };
    let hot = s.hot == Some(target) || s.hot == Some(slope);
    let line = if hot { pal.accent } else { with_alpha(pal.text, 0.85) };
    if f.length > 0 {
        let (u0, u1) = if fade_in { (c.offset, c.offset + f.length) } else { (geom::end_u(c) - f.length, geom::end_u(c)) };
        // Only the part on screen, so a long fade stays smooth zoomed in.
        let (from, to) = ((u0 as f64).max(l.axis.tick(-8.)), (u1 as f64).min(l.axis.tick(l.w + 8.)));
        if to > from {
            let n = (((to - from) * l.axis.px_per_tick() / 4.).ceil() as usize).clamp(2, 400);
            let pts: Vec<Point<Pixels>> = (0..=n)
                .map(|i| {
                    let u = from + (to - from) * i as f64 / n as f64;
                    let along = ((u - u0 as f64) / f.length as f64) as f32;
                    pen.pt(l.axis.x(u), l.gain_y(curve_shape(if fade_in { along } else { 1. - along }, f.curve)))
                })
                .collect();
            let mut shade = pts.clone();
            shade.push(pen.pt(l.axis.x(to), l.gain_y(1.)));
            shade.push(pen.pt(l.axis.x(from), l.gain_y(1.)));
            paint::polygon(window, &shade, shadow(0.3));
            paint::polyline(window, &pts, 1.5, line);
        }
        if let Some((x, y)) = geom::fade_mid(l, c, fade_in) {
            let side = if s.hot == Some(slope) { 9. } else { 7. };
            window.paint_quad(paint::rrect_border(at(pen.o, x - side / 2., y - side / 2., side, side), side / 2., pal.well, line, 1.5));
        }
    }
    // The corner handle.
    let (x, y) = geom::fade_handle(l, c, fade_in);
    let side = if s.hot == Some(target) { 10. } else { 8. };
    let fill = if s.hot == Some(target) { pal.accent } else { pal.text };
    window.paint_quad(paint::rrect_border(at(pen.o, x - side / 2., y - side / 2., side, side), 2., fill, pal.well, 1.));
}

/// The volume envelope: its line over the played region and its breakpoints.
fn envelope(pen: &Pen, window: &mut Window) {
    let pal = p();
    let (s, l) = (pen.s, &pen.l);
    let c = &s.clip;
    let color = mix(s.color, pal.automation, 0.35);
    let (xs, xe) = (l.x(c.offset).max(-4.), l.x(geom::end_u(c)).min(l.w + 4.));
    if xe <= xs {
        return;
    }
    let Some(lane) = c.volume_envelope().filter(|lane| !lane.points.is_empty()) else {
        // No envelope yet: unity, where a double-click starts one.
        window.paint_quad(paint::fill_rect(at(pen.o, xs, l.gain_y(1.), xe - xs, 1.), with_alpha(color, 0.4)));
        return;
    };
    // The breakpoints on screen plus a sample every few pixels, so curved
    // segments are smooth at any zoom.
    let clip_time = |x: f32| ((l.axis.tick(x) - c.offset as f64).round() as Tick).clamp(0, c.length);
    let (t0, t1) = (clip_time(xs), clip_time(xe));
    let mut ticks: Vec<Tick> = (0..=((xe - xs) / 3.).ceil() as usize).map(|i| clip_time(xs + i as f32 * 3.)).collect();
    ticks.extend(lane.points.iter().map(|pt| pt.tick).filter(|t| *t >= t0 && *t <= t1));
    ticks.push(t1);
    ticks.sort_unstable();
    ticks.dedup();
    let pts: Vec<Point<Pixels>> =
        ticks.iter().filter_map(|t| lane.value_at(*t).map(|v| pen.pt(l.x(c.offset + t), l.gain_y(v)))).collect();
    paint::polyline(window, &pts, 1.5, color);
    for (i, pt) in lane.points.iter().enumerate() {
        let Some((x, y)) = geom::point_pos(l, c, pt) else { continue };
        if x < -8. || x > l.w + 8. {
            continue;
        }
        let hot = s.hot == Some(Hit::Point(i));
        let side = if hot { 9. } else { 7. };
        let fill = if hot || s.picked == Some(i) { color } else { pal.well };
        window.paint_quad(paint::rrect_border(at(pen.o, x - side / 2., y - side / 2., side, side), side / 2., fill, color, 1.5));
    }
}

/// The loop brace on the ruler.
fn brace(pen: &Pen, window: &mut Window) {
    let pal = p();
    let (s, l) = (pen.s, &pen.l);
    let Some(lp) = s.clip.looping.filter(|lp| lp.end > lp.start) else { return };
    let (a, b) = (l.x(lp.start), l.x(lp.end));
    if b < 0. || a > l.w {
        return;
    }
    let held = matches!(s.hot, Some(Hit::LoopStart | Hit::LoopEnd | Hit::LoopBody));
    let (va, vb) = (a.max(-4.), b.min(l.w + 4.));
    window.paint_quad(paint::rrect(at(pen.o, va, RULER - 7., vb - va, 5.), 2., with_alpha(pal.accent2, if held { 0.85 } else { 0.55 })));
    for (x, hit) in [(a, Hit::LoopStart), (b - 2., Hit::LoopEnd)] {
        let hot = s.hot == Some(hit) || s.hot == Some(Hit::LoopBody);
        window.paint_quad(paint::fill_rect(at(pen.o, x, RULER - 12., 2., 12.), if hot { pal.accent2 } else { with_alpha(pal.accent2, 0.8) }));
    }
}

/// The start and end markers of the played region.
fn markers(pen: &Pen, window: &mut Window) {
    let pal = p();
    let (s, l) = (pen.s, &pen.l);
    let c = &s.clip;
    for (u, hit, dir) in [(c.offset, Hit::Start, 1.), (geom::end_u(c), Hit::End, -1.)] {
        let x = l.x(u);
        if x < -8. || x > l.w + 8. {
            continue;
        }
        let hot = s.hot == Some(hit);
        let color = if hot { pal.accent } else { with_alpha(pal.text, 0.85) };
        let width = if hot { 2. } else { 1. };
        window.paint_quad(paint::fill_rect(at(pen.o, x - (width - 1.) / 2., 0., width, l.h), color));
        // A flag pointing into the clip.
        paint::polygon(window, &[pen.pt(x, 1.), pen.pt(x + 7. * dir, 1.), pen.pt(x, 9.)], color);
    }
}

/// What the dragged handle reads, in a tag beside it.
fn readout(pen: &Pen, text: &str, hit: Hit, window: &mut Window, cx: &mut App) {
    let pal = p();
    let (s, l) = (pen.s, &pen.l);
    let c = &s.clip;
    let middle = |a: Tick, b: Tick| l.axis.x((a + b) as f64 / 2.);
    let anchor = match hit {
        Hit::Start => Some((l.x(c.offset), HANDLE_Y + 24.)),
        Hit::End => Some((l.x(geom::end_u(c)), HANDLE_Y + 24.)),
        Hit::FadeIn => Some(geom::fade_handle(l, c, true)),
        Hit::FadeOut => Some(geom::fade_handle(l, c, false)),
        Hit::FadeInCurve => geom::fade_mid(l, c, true).or(Some(geom::fade_handle(l, c, true))),
        Hit::FadeOutCurve => geom::fade_mid(l, c, false).or(Some(geom::fade_handle(l, c, false))),
        Hit::Point(i) => geom::envelope(c).get(i).and_then(|pt| geom::point_pos(l, c, pt)),
        Hit::Segment(i) => {
            let pts = geom::envelope(c);
            pts.get(i).zip(pts.get(i + 1)).and_then(|(a, b)| {
                let t = (a.tick + b.tick) / 2;
                c.volume_envelope()?.value_at(t).map(|v| (l.x(c.offset + t), l.gain_y(v)))
            })
        }
        Hit::LoopStart | Hit::LoopEnd | Hit::LoopBody => c.looping.map(|lp| (middle(lp.start, lp.end), RULER + GAIN_TOP)),
        Hit::Ruler | Hit::Body => None,
    };
    let Some((x, y)) = anchor else { return };
    let w = text.chars().count() as f32 * fs(TEXT_SM) * 0.6 + 2. * SP2;
    let h = 20.;
    // Beside the handle, kept on the canvas.
    let left = if x + 12. + w > l.w { x - 12. - w } else { x + 12. }.clamp(SP1, (l.w - w - SP1).max(SP1));
    let top = (y - h - 6.).clamp(RULER + 2., (l.h - h - 2.).max(RULER + 2.));
    window.paint_quad(paint::rrect_border(at(pen.o, left, top, w, h), r_sm(), pal.raised, border_strong(), 1.));
    paint::text(window, cx, text, pen.o.x + px(left + w / 2.), pen.o.y + px(top + 3.), TEXT_SM, pal.text, FontWeight::MEDIUM, TextAlign::Center);
}
