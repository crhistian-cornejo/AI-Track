//! Coordinate maths of the sample editor on plain data: source frames ↔
//! ticks ↔ pixels, where the handles sit, what is under the pointer and
//! where a position snaps to. Nothing here needs a window.
//!
//! The horizontal axis is in **unrolled source ticks** (`u`): the file's own
//! time for a clip that does not loop, and for a looping clip the same time
//! continued past the loop's end, so the repeats lie one after another to
//! the right. The clip plays `offset .. offset + length` on that axis, which
//! makes clip time simply `u - offset`.

use crate::grid::GridSpec;
use aitrack_model::{AudioSource, AutomationPoint, Clip, LoopRegion, PPQ, Tick, TimeSignature, curve_shape};

/// Height of the ruler band at the top of the canvas.
pub const RULER: f32 = 20.;
/// Room above the unity line of the gain scale, where the fade handles sit.
pub const GAIN_TOP: f32 = 16.;
/// Room under the silence line of the gain scale.
pub const GAIN_BOTTOM: f32 = 8.;
/// How far from a handle or breakpoint the pointer still grabs it.
pub const GRAB: f32 = 8.;
/// How far from a marker line the pointer still grabs it.
pub const EDGE: f32 = 5.;
/// Fade handles are drawn this far inside the played region.
pub const HANDLE_INSET: f32 = 4.;
/// Their centre, below the top of the canvas.
pub const HANDLE_Y: f32 = RULER + 7.;
/// A fade shorter than this on screen has no slope handle.
const MID_MIN_PX: f64 = 24.;
/// The envelope line is hit this far above and below it.
const LINE_REACH: f32 = 6.;
/// Positions snap to the file's ends and the loop within this many pixels.
const MAGNET_PX: f64 = 6.;
/// Once grid lines are further apart than this, the grid only pulls nearby.
const GRID_FREE_PX: f64 = 32.;
const GRID_PULL_PX: f64 = 10.;
/// Empty space left of the file when the view shows all of it.
pub const PAD_PX: f32 = 12.;

// ───────────────────────────── Time ─────────────────────────────

/// A file's own time: where its frames fall in source ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeBase {
    pub ticks_per_second: f64,
    pub sample_rate: f64,
}

impl TimeBase {
    /// As [`AudioSource::length_ticks`]: a warped file runs at its own
    /// tempo, any other at the project's.
    pub fn of(source: &AudioSource, project_bpm: f64) -> Self {
        let bpm = match source.warp.original_bpm {
            Some(original) if source.warp.enabled => original,
            _ => project_bpm,
        };
        Self { ticks_per_second: bpm.max(1.) / 60. * PPQ as f64, sample_rate: f64::from(source.sample_rate.max(1)) }
    }

    pub fn frame_to_tick(&self, frame: f64) -> f64 {
        frame / self.sample_rate * self.ticks_per_second
    }

    pub fn tick_to_frame(&self, tick: f64) -> f64 {
        tick / self.ticks_per_second * self.sample_rate
    }

    pub fn seconds(&self, ticks: f64) -> f64 {
        ticks / self.ticks_per_second
    }
}

/// The source position that plays at unrolled position `u`: past the loop's
/// end it wraps back into the loop, as playback does.
pub fn unroll(looping: Option<LoopRegion>, u: f64) -> f64 {
    match looping {
        Some(l) if l.end > l.start && u >= l.end as f64 => l.start as f64 + (u - l.start as f64).rem_euclid((l.end - l.start) as f64),
        _ => u,
    }
}

/// Where the played region ends on the axis.
pub fn end_u(c: &Clip) -> Tick {
    c.offset + c.length
}

/// The end of what there is to show: the file, or the clip when it runs on
/// (a looping clip repeats the file past its end).
pub fn content_end(c: &Clip, source_len: Tick) -> Tick {
    source_len.max(end_u(c)).max(1)
}

// ───────────────────────────── Axis ─────────────────────────────

/// Zoom and scroll: unrolled source ticks ↔ pixels from the canvas' left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    /// Width of a quarter note.
    pub px_per_beat: f64,
    /// The tick at the left edge.
    pub start: f64,
}

impl Axis {
    pub fn px_per_tick(&self) -> f64 {
        self.px_per_beat / PPQ as f64
    }

    pub fn x(&self, tick: f64) -> f32 {
        ((tick - self.start) * self.px_per_tick()) as f32
    }

    pub fn tick(&self, x: f32) -> f64 {
        self.start + f64::from(x) / self.px_per_tick()
    }

    /// The whole of `0..end` in a canvas `width` px wide, with a little air on both sides.
    pub fn fit(width: f32, end: Tick) -> Self {
        let room = f64::from((width - 2. * PAD_PX).max(32.));
        let px_per_beat = room / (end.max(1) as f64 / PPQ as f64);
        let axis = Self { px_per_beat, start: 0. };
        Self { start: -f64::from(PAD_PX) / axis.px_per_tick(), ..axis }
    }

    /// Zoom by `factor` keeping the tick under `x` where it is.
    pub fn zoomed(self, x: f32, factor: f64, min: f64, max: f64) -> Self {
        let under = self.tick(x);
        let px_per_beat = (self.px_per_beat * factor).clamp(min.min(max), max);
        let zoomed = Self { px_per_beat, start: 0. };
        Self { start: under - f64::from(x) / zoomed.px_per_tick(), ..zoomed }
    }

    pub fn scrolled(self, dx: f32) -> Self {
        Self { start: self.start + f64::from(dx) / self.px_per_tick(), ..self }
    }

    /// Keep the view on the content: no further left than the padding, and
    /// at most half a view of empty space past `end` (room to pull the
    /// clip's end out).
    pub fn clamped(self, width: f32, end: Tick) -> Self {
        let ppt = self.px_per_tick();
        let (pad, visible) = (f64::from(PAD_PX) / ppt, f64::from(width.max(1.)) / ppt);
        let last = (end as f64 - visible * 0.5).max(-pad);
        Self { start: self.start.clamp(-pad, last), ..self }
    }
}

/// The canvas: the axis and its size in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub axis: Axis,
    pub w: f32,
    pub h: f32,
}

impl Layout {
    pub fn x(&self, u: Tick) -> f32 {
        self.axis.x(u as f64)
    }

    /// The y range of the gain scale: unity at the top, silence at the bottom.
    pub fn gain_span(&self) -> (f32, f32) {
        let top = RULER + GAIN_TOP;
        (top, (self.h - GAIN_BOTTOM).max(top + 1.))
    }

    pub fn gain_y(&self, gain: f32) -> f32 {
        let (top, bottom) = self.gain_span();
        top + (1. - gain.clamp(0., 1.)) * (bottom - top)
    }

    pub fn gain_at_y(&self, y: f32) -> f32 {
        let (top, bottom) = self.gain_span();
        (1. - (y - top) / (bottom - top)).clamp(0., 1.)
    }
}

// ───────────────────────────── Handles ─────────────────────────────

/// Centre of a fade's corner handle.
pub fn fade_handle(l: &Layout, c: &Clip, fade_in: bool) -> (f32, f32) {
    if fade_in {
        (l.x(c.offset + c.fade_in.length) + HANDLE_INSET, HANDLE_Y)
    } else {
        (l.x(end_u(c) - c.fade_out.length) - HANDLE_INSET, HANDLE_Y)
    }
}

/// The handle in the middle of a fade's slope (bends it); `None` while the
/// fade is too short on screen to have one.
pub fn fade_mid(l: &Layout, c: &Clip, fade_in: bool) -> Option<(f32, f32)> {
    let fade = if fade_in { c.fade_in } else { c.fade_out };
    if fade.length as f64 * l.axis.px_per_tick() < MID_MIN_PX {
        return None;
    }
    let half = fade.length as f64 / 2.;
    let u = if fade_in { c.offset as f64 + half } else { end_u(c) as f64 - half };
    Some((l.axis.x(u), l.gain_y(curve_shape(0.5, fade.curve))))
}

/// The breakpoints of the volume envelope, in clip time.
pub fn envelope(c: &Clip) -> &[AutomationPoint] {
    c.volume_envelope().map_or(&[], |lane| &lane.points)
}

/// Where breakpoint `pt` is drawn; `None` when it lies outside the clip
/// (trimming keeps such points so that revealing the audio brings them back).
pub fn point_pos(l: &Layout, c: &Clip, pt: &AutomationPoint) -> Option<(f32, f32)> {
    (pt.tick >= 0 && pt.tick <= c.length).then(|| (l.x(c.offset + pt.tick), l.gain_y(pt.value)))
}

/// The curvature whose shape passes through `mid` halfway along: the
/// inverse of `curve_shape(0.5, curve)`, so a slope follows the pointer.
pub fn curve_through(mid: f32) -> f32 {
    // shape(0.5) = 1 / (e^(k/2) + 1) with k = 6 * curve.
    let mid = mid.clamp(0.02, 0.98);
    (2. * (1. / mid - 1.).ln() / 6.).clamp(-1., 1.)
}

// ───────────────────────────── Hit-testing ─────────────────────────────

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A fade's corner handle (its length).
    FadeIn,
    FadeOut,
    /// The middle of a fade's slope (its curve).
    FadeInCurve,
    FadeOutCurve,
    /// The start and end markers of the played region.
    Start,
    End,
    /// A breakpoint of the volume envelope, by index.
    Point(usize),
    /// The envelope line between breakpoint `i` and the next.
    Segment(usize),
    LoopStart,
    LoopEnd,
    /// The loop brace between its ends.
    LoopBody,
    /// The ruler band, away from the brace and the markers.
    Ruler,
    /// The waveform itself.
    Body,
}

/// What is at `(x, y)` on the canvas. Where handles overlap, a fade handle
/// wins over a marker, a marker over a breakpoint, a breakpoint over a
/// fade's slope handle and that over the envelope line. In the ruler band
/// the loop brace comes first, then the markers.
pub fn hit_test(l: &Layout, c: &Clip, x: f32, y: f32) -> Hit {
    let (xs, xe) = (l.x(c.offset), l.x(end_u(c)));
    let marker = || {
        let (ds, de) = ((x - xs).abs(), (x - xe).abs());
        match (ds <= EDGE, de <= EDGE) {
            (true, true) => Some(if ds <= de { Hit::Start } else { Hit::End }),
            (true, false) => Some(Hit::Start),
            (false, true) => Some(Hit::End),
            (false, false) => None,
        }
    };
    if y < RULER {
        if let Some(lp) = c.looping.filter(|lp| lp.end > lp.start) {
            let (a, b) = (l.x(lp.start), l.x(lp.end));
            let (da, db) = ((x - a).abs(), (x - b).abs());
            if da.min(db) <= EDGE + 1. {
                return if da < db || (da == db && x < (a + b) / 2.) { Hit::LoopStart } else { Hit::LoopEnd };
            }
            if x > a && x < b {
                return Hit::LoopBody;
            }
        }
        return marker().unwrap_or(Hit::Ruler);
    }
    // Square handles: the larger of the two distances.
    let reach = |p: (f32, f32)| (x - p.0).abs().max((y - p.1).abs());
    let (fi, fo) = (reach(fade_handle(l, c, true)), reach(fade_handle(l, c, false)));
    if fi.min(fo) <= GRAB {
        return if fi <= fo { Hit::FadeIn } else { Hit::FadeOut };
    }
    if let Some(m) = marker() {
        return m;
    }
    let points = envelope(c);
    let nearest = points
        .iter()
        .enumerate()
        .filter_map(|(i, pt)| point_pos(l, c, pt).map(|p| (i, (x - p.0).hypot(y - p.1))))
        .filter(|(_, d)| *d <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((i, _)) = nearest {
        return Hit::Point(i);
    }
    if fade_mid(l, c, true).is_some_and(|p| reach(p) <= GRAB) {
        return Hit::FadeInCurve;
    }
    if fade_mid(l, c, false).is_some_and(|p| reach(p) <= GRAB) {
        return Hit::FadeOutCurve;
    }
    let t = (l.axis.tick(x) - c.offset as f64).round() as Tick;
    if t >= 0
        && t <= c.length
        && let Some(i) = points.windows(2).position(|w| t >= w[0].tick && t < w[1].tick)
        && let Some(v) = c.volume_envelope().and_then(|lane| lane.value_at(t))
        && (l.gain_y(v) - y).abs() <= LINE_REACH
    {
        return Hit::Segment(i);
    }
    Hit::Body
}

// ───────────────────────────── Snapping ─────────────────────────────

/// Snap the unrolled position `u` (`free` = Alt: only round to a tick).
///
/// `magnets` (the file's ends, the loop) pull within a few pixels and win
/// over the grid. The grid is in timeline time: `origin` is the axis
/// position of song tick 0, so a cut on a grid line is on the song's grid.
/// While its lines are close together every position goes to the nearest
/// line; zoomed in past the finest step the lines only pull nearby, leaving
/// the space between them free (single samples are reachable).
pub fn snap(u: f64, grid: &GridSpec, origin: Tick, px_per_tick: f64, magnets: &[Tick], free: bool) -> Tick {
    let raw = u.round() as Tick;
    if free {
        return raw;
    }
    let px = |a: Tick| (a - raw).abs() as f64 * px_per_tick;
    if let Some(m) = magnets.iter().copied().min_by_key(|m| (m - raw).abs())
        && px(m) <= MAGNET_PX
    {
        return m;
    }
    let line = origin + grid.round(raw - origin);
    if grid.step as f64 * px_per_tick <= GRID_FREE_PX || px(line) <= GRID_PULL_PX { line } else { raw }
}

// ───────────────────────────── Read-outs ─────────────────────────────

/// A linear gain as decibels: "-6.0 dB", "-inf dB".
pub fn format_gain(gain: f32) -> String {
    if gain <= 0.0003 { "-inf dB".into() } else { format!("{:+.1} dB", 20. * gain.log10()) }
}

pub fn db_to_gain(db: f32) -> f32 {
    if db <= -70. { 0. } else { 10f32.powf(db / 20.) }
}

/// A length as bars.beats.sixteenths, the way Live writes it: "2.0.0".
pub fn format_length(ticks: Tick, sig: TimeSignature) -> String {
    let bar = sig.bar_ticks().max(1);
    let beat = (PPQ * 4 / i64::from(sig.denominator.max(1))).max(1);
    let ticks = ticks.max(0);
    let in_bar = ticks % bar;
    format!("{}.{}.{}", ticks / bar, in_bar / beat, (in_bar % beat) / (PPQ / 4))
}

pub fn format_seconds(seconds: f64) -> String {
    if seconds < 10. { format!("{seconds:.3} s") } else { format!("{seconds:.2} s") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid;
    use aitrack_model::{AutomationLane, AutomationTarget, Color, Fade, SourceId, Warp};

    const BEAT: Tick = PPQ;
    const FOUR_FOUR: TimeSignature = TimeSignature { numerator: 4, denominator: 4 };

    fn source(frames: u64, rate: u32) -> AudioSource {
        AudioSource { path: "a.wav".into(), hash: None, sample_rate: rate, channels: 2, frames, warp: Warp::default(), key: None }
    }

    /// 100 px per beat, the file starting 20 px in; a 400 × 200 px canvas.
    fn layout() -> Layout {
        Layout { axis: Axis { px_per_beat: 100., start: -0.2 * BEAT as f64 }, w: 400., h: 200. }
    }

    /// A clip that plays beats 1..3 of its file.
    fn clip() -> Clip {
        Clip { offset: BEAT, ..Clip::new("c", Color(0), SourceId(1), 2 * BEAT) }
    }

    fn with_points(mut c: Clip, points: &[(Tick, f32)]) -> Clip {
        let points = points.iter().map(|&(tick, value)| AutomationPoint { tick, value, curve: 0. }).collect();
        c.envelopes.push(AutomationLane { points, ..AutomationLane::new(AutomationTarget::Volume) });
        c
    }

    #[test]
    fn frames_ticks_and_pixels_round_trip_at_any_zoom() {
        // 48 kHz at 120 BPM: a beat is 24 000 frames.
        let base = TimeBase::of(&source(96_000, 48_000), 120.);
        assert_eq!(base.frame_to_tick(24_000.), BEAT as f64);
        assert_eq!(base.tick_to_frame(2. * BEAT as f64), 48_000.);
        assert_eq!(base.tick_to_frame(1.), 25.);
        assert_eq!(TimeBase { sample_rate: 24_000., ..base }.tick_to_frame(1.), 12.5, "the same file decoded at half the rate");
        assert_eq!(base.seconds(4. * BEAT as f64), 2.);
        // The whole file is as long as the model says.
        let src = source(96_000, 48_000);
        assert_eq!(base.frame_to_tick(96_000.).round() as Tick, src.length_ticks(120.));
        // A warped file runs at its own tempo whatever the project's is.
        let mut warped = source(96_000, 48_000);
        warped.warp = Warp { enabled: true, original_bpm: Some(60.), ..Warp::default() };
        let slow = TimeBase::of(&warped, 120.);
        assert_eq!(slow.frame_to_tick(96_000.).round() as Tick, warped.length_ticks(120.));
        assert_eq!(slow.frame_to_tick(48_000.), BEAT as f64);

        for px_per_beat in [0.5, 7., 64., 320., 5_000., 190_000.] {
            for start in [-500., 0., 12_345.6, 9_000_000.] {
                let axis = Axis { px_per_beat, start };
                for frame in [0., 1., 4_799., 24_000., 1_000_000.] {
                    let x = axis.x(base.frame_to_tick(frame) - 12_000. + start);
                    let back = base.tick_to_frame(axis.tick(x) + 12_000. - start);
                    // A pixel is the limit of what a position on screen can tell apart.
                    let frames_per_px = base.tick_to_frame(1. / axis.px_per_tick());
                    assert!((back - frame).abs() <= frames_per_px.max(1e-3), "{frame} came back as {back} at zoom {px_per_beat}");
                }
                // Anywhere on a wide canvas, a pixel maps to a tick and back to itself.
                for x in [0., 1., 250.5, 1_999., 3_840.] {
                    let err = (axis.x(axis.tick(x)) - x).abs();
                    assert!(err < 0.02, "x {x} is {err} px off after a round trip at zoom {px_per_beat}");
                }
            }
        }
    }

    #[test]
    fn zoom_keeps_the_pointer_in_place_and_fit_shows_everything() {
        let axis = Axis { px_per_beat: 64., start: 3_000. };
        let under = axis.tick(250.);
        let zoomed = axis.zoomed(250., 3., 1., 10_000.);
        assert_eq!(zoomed.px_per_beat, 192.);
        assert!((zoomed.tick(250.) - under).abs() < 1e-6);
        // Limits hold, and the pointer still does not move.
        let most = axis.zoomed(250., 1e9, 1., 10_000.);
        assert_eq!(most.px_per_beat, 10_000.);
        assert!((most.tick(250.) - under).abs() < 1e-6);
        assert_eq!(axis.zoomed(250., 1e-9, 1., 10_000.).px_per_beat, 1.);

        let fit = Axis::fit(800., 8 * BEAT);
        assert!((fit.x(0.) - PAD_PX).abs() < 1e-3);
        assert!((fit.x(8. * BEAT as f64) - (800. - PAD_PX)).abs() < 1e-2);
        // Scrolling moves by pixels and stops at the content.
        assert!((fit.scrolled(40.).x(0.) - (PAD_PX - 40.)).abs() < 1e-3);
        assert_eq!(fit.scrolled(-5_000.).clamped(800., 8 * BEAT), fit);
        let end = fit.scrolled(50_000.).clamped(800., 8 * BEAT);
        assert!((end.x(8. * BEAT as f64) - 400.).abs() < 1e-2, "half a view of room past the end");
    }

    #[test]
    fn a_looping_clip_unrolls_its_repeats() {
        let lp = Some(LoopRegion { start: BEAT, end: 3 * BEAT });
        assert_eq!(unroll(lp, 0.), 0.);
        assert_eq!(unroll(lp, 2.5 * BEAT as f64), 2.5 * BEAT as f64);
        assert_eq!(unroll(lp, 3. * BEAT as f64), BEAT as f64);
        assert_eq!(unroll(lp, 6.5 * BEAT as f64), 2.5 * BEAT as f64);
        assert_eq!(unroll(None, 6.5 * BEAT as f64), 6.5 * BEAT as f64);
        // An empty region does not loop.
        assert_eq!(unroll(Some(LoopRegion { start: BEAT, end: BEAT }), 5_000.), 5_000.);
        let mut c = clip();
        assert_eq!((end_u(&c), content_end(&c, 8 * BEAT)), (3 * BEAT, 8 * BEAT));
        c.length = 20 * BEAT;
        assert_eq!(content_end(&c, 8 * BEAT), 21 * BEAT);
    }

    #[test]
    fn the_gain_scale_runs_from_unity_at_the_top_to_silence() {
        let l = layout();
        assert_eq!((l.gain_y(1.), l.gain_y(0.)), (RULER + GAIN_TOP, 200. - GAIN_BOTTOM));
        for g in [0., 0.25, 0.5, 1.] {
            assert!((l.gain_at_y(l.gain_y(g)) - g).abs() < 1e-5);
        }
        assert_eq!((l.gain_at_y(0.), l.gain_at_y(500.)), (1., 0.));
    }

    #[test]
    fn handles_sit_where_the_clip_puts_them() {
        let l = layout();
        let mut c = clip();
        // The played region runs from x = 120 to x = 320.
        assert_eq!((l.x(c.offset), l.x(end_u(&c))), (120., 320.));
        assert_eq!(fade_handle(&l, &c, true), (120. + HANDLE_INSET, HANDLE_Y));
        assert_eq!(fade_handle(&l, &c, false), (320. - HANDLE_INSET, HANDLE_Y));
        assert_eq!(fade_mid(&l, &c, true), None, "no fade, no slope handle");
        c.fade_in = Fade { length: BEAT / 2, curve: 0. };
        c.fade_out = Fade { length: BEAT, curve: 1. };
        assert_eq!(fade_handle(&l, &c, true).0, 170. + HANDLE_INSET);
        assert_eq!(fade_handle(&l, &c, false).0, 220. - HANDLE_INSET);
        assert_eq!(fade_mid(&l, &c, true), Some((145., l.gain_y(0.5))));
        let (x, y) = fade_mid(&l, &c, false).unwrap();
        assert_eq!(x, 270.);
        assert!(y > l.gain_y(0.1), "a fade that bends down has its middle low");
        // Breakpoints outside the clip are kept but not drawn.
        let c = with_points(c, &[(-BEAT, 0.5), (0, 1.), (BEAT, 0.5), (5 * BEAT, 0.)]);
        let drawn: Vec<_> = envelope(&c).iter().map(|pt| point_pos(&l, &c, pt)).collect();
        assert_eq!(drawn, vec![None, Some((120., l.gain_y(1.))), Some((220., l.gain_y(0.5))), None]);
        assert!(envelope(&clip()).is_empty());
    }

    #[test]
    fn curve_through_inverts_the_curve_shape() {
        for curve in [-1., -0.5, 0., 0.3, 1.] {
            assert!((curve_through(curve_shape(0.5, curve)) - curve).abs() < 1e-3, "curve {curve}");
        }
        assert_eq!((curve_through(0.), curve_through(1.)), (1., -1.));
        assert!(curve_through(0.9) < 0. && curve_through(0.2) > 0., "lifting the middle is a negative curve");
    }

    #[test]
    fn hit_testing_prefers_fade_handles_then_markers_then_breakpoints() {
        let l = layout();
        // A breakpoint at the very start of the clip, at unity: it sits on
        // the start marker, right under the fade-in handle.
        let c = with_points(clip(), &[(0, 1.), (BEAT, 0.5), (2 * BEAT, 1.)]);
        let (hx, hy) = fade_handle(&l, &c, true);
        let (px, py) = point_pos(&l, &c, &envelope(&c)[0]).unwrap();
        assert!((hx - px).abs() <= GRAB && (hy - py).abs() <= GRAB + GRAB, "the three overlap");
        // On the handle: the fade.
        assert_eq!(hit_test(&l, &c, hx, hy), Hit::FadeIn);
        assert_eq!(hit_test(&l, &c, 120., HANDLE_Y + GRAB), Hit::FadeIn);
        // On the breakpoint, which is also on the marker line: the marker.
        assert_eq!(hit_test(&l, &c, px, py + 2.), Hit::Start);
        assert_eq!(hit_test(&l, &c, 120., 150.), Hit::Start);
        // Beside the line, still within reach of the breakpoint: the breakpoint.
        assert_eq!(hit_test(&l, &c, px + EDGE + 1., py + 3.), Hit::Point(0));
        // The same at the other end.
        assert_eq!(hit_test(&l, &c, 320. - HANDLE_INSET, HANDLE_Y), Hit::FadeOut);
        assert_eq!(hit_test(&l, &c, 322., 100.), Hit::End);
        assert_eq!(hit_test(&l, &c, 320. - EDGE - 2., l.gain_y(1.) + 3.), Hit::Point(2));
        // Away from everything.
        assert_eq!(hit_test(&l, &c, 220., l.gain_y(0.5)), Hit::Point(1));
        assert_eq!(hit_test(&l, &c, 170., l.gain_y(0.75) + 2.), Hit::Segment(0));
        assert_eq!(hit_test(&l, &c, 270., l.gain_y(0.75) - 3.), Hit::Segment(1));
        assert_eq!(hit_test(&l, &c, 170., 180.), Hit::Body);
        assert_eq!(hit_test(&l, &c, 60., 100.), Hit::Body);
        assert_eq!(hit_test(&l, &c, 380., l.gain_y(1.)), Hit::Body, "no envelope line outside the clip");
        // Every handle has at least 8 px of reach.
        for (dx, dy) in [(GRAB, 0.), (-GRAB, 0.), (0., GRAB), (GRAB, GRAB), (0., RULER - HANDLE_Y)] {
            assert_eq!(hit_test(&l, &c, hx + dx, hy + dy), Hit::FadeIn);
        }
        for d in [-GRAB, GRAB] {
            assert_eq!(hit_test(&l, &c, 220. + d, l.gain_y(0.5)), Hit::Point(1));
            assert_eq!(hit_test(&l, &c, 220., l.gain_y(0.5) + d), Hit::Point(1));
        }
        for d in [-EDGE, EDGE] {
            assert_eq!(hit_test(&l, &c, 120. + d, 120.), Hit::Start);
            assert_eq!(hit_test(&l, &c, 320. + d, 120.), Hit::End);
        }
    }

    #[test]
    fn fade_slopes_and_the_ruler_have_their_own_targets() {
        let l = layout();
        let mut c = clip();
        c.fade_in = Fade { length: BEAT, curve: 0. };
        c.fade_out = Fade { length: BEAT / 2, curve: 0. };
        let (mx, my) = fade_mid(&l, &c, true).unwrap();
        assert_eq!(hit_test(&l, &c, mx + 3., my - 3.), Hit::FadeInCurve);
        let (mx, my) = fade_mid(&l, &c, false).unwrap();
        assert_eq!(hit_test(&l, &c, mx, my), Hit::FadeOutCurve);
        // A breakpoint on the slope handle wins.
        let c2 = with_points(c.clone(), &[(BEAT / 2, 0.5)]);
        let (mx, my) = fade_mid(&l, &c2, true).unwrap();
        assert_eq!(hit_test(&l, &c2, mx, my), Hit::Point(0));
        // The fade handles moved in with their fades.
        assert_eq!(hit_test(&l, &c, 220. + HANDLE_INSET, HANDLE_Y), Hit::FadeIn);
        assert_eq!(hit_test(&l, &c, 270. - HANDLE_INSET, HANDLE_Y), Hit::FadeOut);
        assert_eq!(hit_test(&l, &c, 120., HANDLE_Y), Hit::Start, "the corner is the marker's again");

        // Ruler band: markers when the clip does not loop.
        assert_eq!(hit_test(&l, &c, 121., 5.), Hit::Start);
        assert_eq!(hit_test(&l, &c, 318., 5.), Hit::End);
        assert_eq!(hit_test(&l, &c, 200., 5.), Hit::Ruler);
        // Looping over what the clip shows puts the brace's ends on the
        // markers: in the ruler the brace wins, below it the markers do.
        c.looping = Some(LoopRegion { start: BEAT, end: 3 * BEAT });
        assert_eq!(hit_test(&l, &c, 121., 5.), Hit::LoopStart);
        assert_eq!(hit_test(&l, &c, 318., 5.), Hit::LoopEnd);
        assert_eq!(hit_test(&l, &c, 200., 5.), Hit::LoopBody);
        assert_eq!(hit_test(&l, &c, 60., 5.), Hit::Ruler);
        assert_eq!(hit_test(&l, &c, 121., 100.), Hit::Start);
        // A longer clip: its end marker is past the brace.
        c.length = 3 * BEAT;
        assert_eq!(hit_test(&l, &c, 420., 5.), Hit::End);
        assert_eq!(hit_test(&l, &c, 320., 5.), Hit::LoopEnd);
    }

    #[test]
    fn snapping_follows_the_grid_the_magnets_and_alt() {
        // 100 px per beat: sixteenths are 25 px apart.
        let l = layout();
        let ppt = l.axis.px_per_tick();
        let g = grid::adaptive(100., FOUR_FOUR, 0, false);
        assert_eq!(g.step, BEAT / 4);
        assert_eq!(snap(250., &g, 0, ppt, &[], false), 240);
        assert_eq!(snap(130., &g, 0, ppt, &[], false), 240);
        assert_eq!(snap(110., &g, 0, ppt, &[], false), 0);
        assert_eq!(snap(110.4, &g, 0, ppt, &[], true), 110, "Alt only rounds to a tick");
        // The grid is the song's: a clip placed off the grid snaps to it.
        assert_eq!(snap(250., &g, 100, ppt, &[], false), 340);
        assert_eq!(snap(250., &g, -100, ppt, &[], false), 140);
        // The end of the file pulls from a few pixels away, grid line or not.
        let end = 7_777;
        assert_eq!(snap(end as f64 + 40., &g, 0, ppt, &[0, end], false), end);
        assert_eq!(snap(end as f64 - 50., &g, 0, ppt, &[0, end], false), end);
        assert_eq!(snap(end as f64 - 90., &g, 0, ppt, &[0, end], false), 7_680);
        // Zoomed in past the finest step (1/128 = 30 ticks, 400 px here):
        // lines pull nearby and leave the rest free.
        let ppt = 400. / 30.;
        let g = grid::adaptive((ppt * BEAT as f64) as f32, FOUR_FOUR, 0, false);
        assert_eq!(g.step, 30);
        assert_eq!(snap(960.4, &g, 0, ppt, &[], false), 960);
        assert_eq!(snap(975., &g, 0, ppt, &[], false), 975);
        assert_eq!(snap(989.6, &g, 0, ppt, &[], false), 990);
    }

    #[test]
    fn read_outs() {
        assert_eq!(format_gain(1.), "+0.0 dB");
        assert_eq!(format_gain(0.5), "-6.0 dB");
        assert_eq!(format_gain(0.), "-inf dB");
        assert!((db_to_gain(-6.) - 0.501).abs() < 1e-3);
        assert_eq!((db_to_gain(0.), db_to_gain(-70.)), (1., 0.));
        assert_eq!(format_length(8 * BEAT, FOUR_FOUR), "2.0.0");
        assert_eq!(format_length(5 * BEAT + BEAT / 2, FOUR_FOUR), "1.1.2");
        assert_eq!(format_length(7 * BEAT / 2, TimeSignature { numerator: 6, denominator: 8 }), "1.1.0");
        assert_eq!(format_seconds(1.5), "1.500 s");
        assert_eq!(format_seconds(75.25), "75.25 s");
    }
}
