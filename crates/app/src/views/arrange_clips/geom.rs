//! Geometry of a clip in an arrangement lane, on plain data: its rectangle,
//! where the fade handles sit, what is under the pointer, and which source
//! frames each pixel column of its waveform shows. Nothing here needs a
//! window.
//!
//! Positions are in pixels from the top-left corner of the lane canvas.
//! Time on a clip is **clip time**: ticks since its placement starts on the
//! timeline. The source position that plays there is `offset + clip time`,
//! wrapped into the loop as playback does (`sample_editor::geom::unroll`).

use crate::views::sample_editor::geom::{db_to_gain, unroll};
use crate::waveform::{self, Peaks};
use aitrack_model::{Clip, Fade, LoopRegion, Tick, curve_shape};

/// Height of a clip's title strip.
pub const HEAD: f32 = 14.;
/// Each edge of a clip grabs this far into it.
pub const EDGE: f32 = 8.;
/// Side of a fade handle as drawn.
pub const HANDLE: f32 = 8.;
/// How far from a handle's centre the pointer still grabs it.
pub const GRAB: f32 = 8.;
/// A fade handle's centre sits this far inside the end of its fade.
const INSET: f32 = 6.;
/// Clips narrower than this show no fade handles: their edges win.
pub const HANDLES_MIN_W: f32 = 48.;
/// Clips whose body is lower than this show no fade handles either.
const HANDLES_MIN_H: f32 = 16.;
/// A fade narrower than this on screen has no slope handle.
const SLOPE_MIN_PX: f32 = 24.;
/// Two channels are drawn apart once the body is at least this tall.
pub const SPLIT_MIN_H: f32 = 44.;

/// What a press on a clip does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// A fade's corner handle (its length).
    FadeIn,
    FadeOut,
    /// The handle in the middle of a fade's slope (its curve).
    FadeInCurve,
    FadeOutCurve,
    /// The left edge: trims or reveals the clip's start.
    Start,
    /// The right edge: resizes, and loops an audio clip past its file.
    End,
    /// Anywhere else: moves the clip.
    Body,
}

/// A clip as it lies in its lane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipBox {
    /// Left edge and width of the clip on the timeline.
    pub x: f32,
    pub w: f32,
    /// Top and height of the clip in its lane.
    pub y: f32,
    pub h: f32,
    pub px_per_tick: f32,
    pub length: Tick,
    pub fade_in: Fade,
    pub fade_out: Fade,
    /// Audio clips have fades; a notes clip only has its edges.
    pub audio: bool,
}

impl ClipBox {
    /// The clip placed at `start` in a lane whose top is `lane_top` and
    /// which is `lane_h` high.
    pub fn of(clip: &Clip, audio: bool, start: Tick, lane_top: f32, lane_h: f32, px_per_tick: f32, scroll_x: f32) -> Self {
        Self {
            x: start as f32 * px_per_tick - scroll_x,
            w: clip.length as f32 * px_per_tick,
            y: lane_top + 2.,
            h: lane_h - 4.,
            px_per_tick,
            length: clip.length,
            fade_in: clip.fade_in,
            fade_out: clip.fade_out,
            audio,
        }
    }

    /// Top and height of the part under the title, where the content is drawn.
    pub fn body(&self) -> (f32, f32) {
        (self.y + HEAD + 2., self.h - HEAD - 4.)
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// Whether the clip has room for its fade handles.
    pub fn has_handles(&self) -> bool {
        self.audio && self.w >= HANDLES_MIN_W && self.body().1 >= HANDLES_MIN_H
    }

    /// How far each edge grabs into the clip: [`EDGE`], less on a clip so
    /// narrow that its middle third would not be left to move it by.
    pub fn edge(&self) -> f32 {
        (self.w / 3.).min(EDGE)
    }

    /// Where a fade meets full level.
    pub fn fade_end(&self, fade_in: bool) -> f32 {
        if fade_in {
            self.x + (self.fade_in.length as f32 * self.px_per_tick).clamp(0., self.w)
        } else {
            self.x + self.w - (self.fade_out.length as f32 * self.px_per_tick).clamp(0., self.w)
        }
    }

    /// Centre of a fade's corner handle: at the top of the body, just
    /// inside the end of the fade, and always inside the clip.
    pub fn handle(&self, fade_in: bool) -> (f32, f32) {
        let (left, right) = (self.x + INSET, self.x + self.w - INSET);
        let x = if fade_in { (self.fade_end(true) + INSET).min(right) } else { (self.fade_end(false) - INSET).max(left) };
        (x, self.body().0 + HANDLE / 2. + 1.)
    }

    /// The handle in the middle of a fade's slope; `None` while the fade is
    /// too short on screen to have one.
    pub fn slope(&self, fade_in: bool) -> Option<(f32, f32)> {
        let (fade, end) = if fade_in { (self.fade_in, self.x) } else { (self.fade_out, self.x + self.w) };
        let px = (self.fade_end(fade_in) - end).abs();
        (self.has_handles() && fade.length > 0 && px >= SLOPE_MIN_PX).then(|| ((self.fade_end(fade_in) + end) / 2., self.gain_y(curve_shape(0.5, fade.curve))))
    }

    /// The gain scale of the body: unity at its top, silence at its bottom.
    pub fn gain_y(&self, gain: f32) -> f32 {
        let (top, h) = self.body();
        top + (1. - gain.clamp(0., 1.)) * h
    }

    pub fn gain_at_y(&self, y: f32) -> f32 {
        let (top, h) = self.body();
        (1. - (y - top) / h.max(1.)).clamp(0., 1.)
    }

    /// What a press at `(x, y)` does; `None` outside the clip. Where targets
    /// overlap, a fade handle wins over an edge, an edge over a slope handle
    /// and that over the body. A clip too narrow for handles has only its
    /// edges and its body.
    pub fn hit(&self, x: f32, y: f32) -> Option<Zone> {
        if !self.contains(x, y) {
            return None;
        }
        // Square handles: the larger of the two distances.
        let reach = |p: (f32, f32)| (x - p.0).abs().max((y - p.1).abs());
        if self.has_handles() {
            let (fi, fo) = (reach(self.handle(true)), reach(self.handle(false)));
            if fi.min(fo) <= GRAB {
                return Some(if fi <= fo { Zone::FadeIn } else { Zone::FadeOut });
            }
        }
        let edge = self.edge();
        if x - self.x < edge {
            return Some(Zone::Start);
        }
        if self.x + self.w - x < edge {
            return Some(Zone::End);
        }
        if self.slope(true).is_some_and(|p| reach(p) <= GRAB) {
            return Some(Zone::FadeInCurve);
        }
        if self.slope(false).is_some_and(|p| reach(p) <= GRAB) {
            return Some(Zone::FadeOutCurve);
        }
        Some(Zone::Body)
    }
}

/// The topmost clip under `(x, y)` and what a press there does. `boxes` are
/// in paint order: the last one is on top.
pub fn topmost(boxes: &[ClipBox], x: f32, y: f32) -> Option<(usize, Zone)> {
    boxes.iter().enumerate().rev().find_map(|(i, b)| b.hit(x, y).map(|zone| (i, zone)))
}

// ───────────────────────────── Waveform ─────────────────────────────

/// Clip time → source frames of one clip at one zoom.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveMap {
    /// Where in the source the clip starts, and its loop.
    pub offset: Tick,
    pub looping: Option<LoopRegion>,
    /// Frames of the decoded file per source tick (a warped file runs at its
    /// own tempo: `sample_editor::geom::TimeBase`).
    pub frames_per_tick: f64,
    /// Width of a pixel column in ticks.
    pub ticks_per_px: f64,
    /// Source ticks played per clip tick: transpose and detune (Re-Pitch).
    pub ratio: f64,
    /// Frames of the file when it plays reversed: frames are mirrored.
    pub mirror: Option<f64>,
}

impl WaveMap {
    pub fn of(clip: &Clip, frames_per_tick: f64, px_per_tick: f64) -> Self {
        Self {
            offset: clip.offset,
            looping: clip.looping,
            frames_per_tick,
            ticks_per_px: 1. / px_per_tick.max(1e-9),
            ratio: 1.,
            mirror: None,
        }
    }

    /// As playback reads the source: `ratio` times faster (transpose), and
    /// backwards through a file of `mirror` frames.
    pub fn pitched(self, ratio: f64, mirror: Option<f64>) -> Self {
        Self {
            ratio: ratio.max(1e-6),
            mirror,
            ..self
        }
    }

    /// The source frames shown by the column at clip time `t`, as the file
    /// stores them (mirrored back when the clip plays reversed).
    pub fn file_frames(&self, t: f64) -> (f64, f64) {
        let (from, to) = self.frames(t);
        match self.mirror {
            Some(n) => (n - to, n - from),
            None => (from, to),
        }
    }

    /// The source frames `from..to` shown by the pixel column that starts at
    /// clip time `t`. Past the loop's end the position wraps back into the
    /// loop; a column never reads past the end of the loop it is in, and one
    /// that is wider than the whole loop shows all of it.
    pub fn frames(&self, t: f64) -> (f64, f64) {
        let u = self.offset as f64 + t * self.ratio;
        let width = self.ticks_per_px * self.ratio * self.frames_per_tick;
        let from = unroll(self.looping, u) * self.frames_per_tick;
        match self.looping.filter(|l| l.end > l.start) {
            Some(l) => {
                let (start, end) = (l.start as f64 * self.frames_per_tick, l.end as f64 * self.frames_per_tick);
                if u >= l.end as f64 && width >= end - start { (start, end) } else { (from, (from + width).min(end).max(from + width.min(1.))) }
            }
            None => (from, from + width),
        }
    }
}

/// Fill `out` with one `(min, max)` per pixel column of `clip`'s waveform as
/// it will sound: column `i` starts at clip time `t0 + i` pixels, and its
/// peaks are scaled by the clip gain, the fades and the volume envelope.
/// Columns outside the file are `(0, 0)`; neighbours are made to touch.
pub fn columns(peaks: &Peaks, channel: Option<usize>, clip: &Clip, map: &WaveMap, t0: f64, count: usize, out: &mut Vec<(f32, f32)>) {
    out.clear();
    out.reserve(count);
    let clip_gain = db_to_gain(clip.gain_db);
    let mut prev = None;
    for i in 0..count {
        let t = t0 + i as f64 * map.ticks_per_px;
        let (from, to) = map.file_frames(t);
        let col = peaks.range(channel, from, to);
        let (lo, hi) = waveform::join(col, prev);
        prev = col;
        // The gain in the middle of the column, inside the clip.
        let mid = ((t + map.ticks_per_px / 2.).round() as Tick).clamp(0, clip.length.max(0));
        let gain = clip_gain * clip.gain_at(mid);
        out.push(((lo * gain).clamp(-1., 1.), (hi * gain).clamp(-1., 1.)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Color, PPQ, SourceId};

    const BEAT: Tick = PPQ;

    /// A 200 × 60 px audio clip at (100, 10), 100 px per beat.
    fn boxed(w: f32) -> ClipBox {
        ClipBox { x: 100., w, y: 10., h: 60., px_per_tick: 100. / BEAT as f32, length: (w / 100. * BEAT as f32) as Tick, fade_in: Fade::default(), fade_out: Fade::default(), audio: true }
    }

    #[test]
    fn a_fade_handle_wins_over_an_edge_and_an_edge_over_the_body() {
        let b = boxed(200.);
        let (top, _) = b.body();
        assert_eq!(b.hit(50., 30.), None, "left of the clip");
        assert_eq!(b.hit(150., 5.), None, "above it");
        // The top corners of the body are the fade handles, even over the edge.
        assert_eq!(b.hit(101., top + 4.), Some(Zone::FadeIn));
        assert_eq!(b.hit(112., top + 10.), Some(Zone::FadeIn));
        assert_eq!(b.hit(298., top + 4.), Some(Zone::FadeOut));
        // Below the handles the edges take over, 8 px into the clip.
        assert_eq!(b.hit(101., top + 30.), Some(Zone::Start));
        assert_eq!(b.hit(107.5, top + 30.), Some(Zone::Start));
        assert_eq!(b.hit(108.5, top + 30.), Some(Zone::Body));
        assert_eq!(b.hit(299., top + 30.), Some(Zone::End));
        assert_eq!(b.hit(292.5, top + 30.), Some(Zone::End));
        assert_eq!(b.hit(291.5, top + 30.), Some(Zone::Body));
        // The title strip moves the clip, and its ends are edges too.
        assert_eq!(b.hit(200., b.y + 3.), Some(Zone::Body));
        assert_eq!(b.hit(102., b.y + 3.), Some(Zone::Start));
        // Every target is at least 8 px across.
        assert!(b.edge() >= 8. && 2. * GRAB >= 8.);
    }

    #[test]
    fn a_narrow_or_low_clip_keeps_its_edges_and_hides_the_handles() {
        let narrow = boxed(40.);
        let (top, _) = narrow.body();
        assert!(!narrow.has_handles());
        assert_eq!(narrow.hit(103., top + 4.), Some(Zone::Start), "the corner is the edge again");
        assert_eq!(narrow.hit(120., top + 4.), Some(Zone::Body));
        assert_eq!(narrow.hit(137., top + 4.), Some(Zone::End));
        // A sliver still has a middle to move it by.
        let sliver = boxed(9.);
        assert_eq!(sliver.edge(), 3.);
        assert_eq!((sliver.hit(101., 40.), sliver.hit(104.5, 40.), sliver.hit(108., 40.)), (Some(Zone::Start), Some(Zone::Body), Some(Zone::End)));
        // A notes clip and a clip in a low lane have no fades to grab.
        let notes = ClipBox { audio: false, ..boxed(200.) };
        assert_eq!(notes.hit(104., notes.body().0 + 4.), Some(Zone::Start));
        assert_eq!(notes.hit(120., notes.body().0 + 4.), Some(Zone::Body));
        let low = ClipBox { h: 30., ..boxed(200.) };
        assert!(!low.has_handles());
    }

    #[test]
    fn handles_follow_their_fades_and_the_slope_handle_needs_room() {
        let mut b = boxed(200.);
        assert_eq!((b.handle(true).0, b.handle(false).0), (106., 294.));
        assert_eq!((b.slope(true), b.slope(false)), (None, None), "no fade, no slope");
        b.fade_in.length = BEAT / 2;
        b.fade_out.length = BEAT;
        assert_eq!((b.fade_end(true), b.fade_end(false)), (150., 200.));
        assert_eq!((b.handle(true).0, b.handle(false).0), (156., 194.));
        // Halfway along a linear fade the gain is a half.
        let (top, h) = b.body();
        assert_eq!(b.slope(true), Some((125., top + h / 2.)));
        assert_eq!(b.slope(false), Some((250., top + h / 2.)));
        assert_eq!(b.hit(125., top + h / 2. + 3.), Some(Zone::FadeInCurve));
        assert_eq!(b.hit(250., top + h / 2.), Some(Zone::FadeOutCurve));
        assert_eq!(b.hit(156., top + 5.), Some(Zone::FadeIn), "the corner handle moved with the fade");
        assert_eq!(b.hit(106., top + 5.), Some(Zone::Start), "and left the corner to the edge");
        // A bent fade moves its slope handle; a short one has none.
        b.fade_in.curve = 1.;
        assert!(b.slope(true).unwrap().1 > top + h * 0.8);
        b.fade_in.length = BEAT / 8;
        assert_eq!(b.slope(true), None);
        // A fade over the whole clip keeps its handle inside it.
        b.fade_in.length = 4 * BEAT;
        assert_eq!(b.handle(true).0, 294.);
        // The gain scale round-trips.
        assert!((b.gain_at_y(b.gain_y(0.25)) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn the_topmost_clip_is_the_one_painted_last() {
        let under = boxed(200.);
        let over = ClipBox { x: 250., ..boxed(100.) };
        assert_eq!(topmost(&[under, over], 270., 50.), Some((1, Zone::Body)));
        assert_eq!(topmost(&[under, over], 253., 50.), Some((1, Zone::Start)), "the edge of the clip on top, not the body under it");
        assert_eq!(topmost(&[under, over], 200., 50.), Some((0, Zone::Body)));
        assert_eq!(topmost(&[under, over], 400., 50.), None);
    }

    /// A ramp from -1 to 1 over `frames`: the value of a sample tells its frame.
    fn ramp(frames: usize) -> Peaks {
        Peaks::from_planar(&[(0..frames).map(|i| -1. + 2. * i as f32 / (frames - 1) as f32).collect()], 48_000)
    }

    /// How far a column of 1000 frames may reach past its ends: the peaks
    /// answer it from buckets of 256 frames.
    const SLACK: f64 = 300.;

    fn frame_of(value: f32, frames: usize) -> f64 {
        f64::from(value + 1.) / 2. * (frames - 1) as f64
    }

    #[test]
    fn transposed_and_reversed_columns_read_what_plays() {
        let clip = Clip {
            offset: 200,
            ..Clip::new("c", Color(0), SourceId(1), 400)
        };
        // An octave up reads the file twice as fast from the same start.
        let up = WaveMap::of(&clip, 100., 0.1).pitched(2., None);
        assert_eq!(up.frames(0.), (20_000., 22_000.));
        assert_eq!(up.frames(50.), (30_000., 32_000.));
        // Reversed, the same columns come from the far end of the file.
        let back = WaveMap::of(&clip, 100., 0.1).pitched(1., Some(100_000.));
        assert_eq!(back.file_frames(0.), (79_000., 80_000.));
    }

    #[test]
    fn columns_of_a_clip_with_an_offset_start_at_that_offset() {
        // 100 frames per tick, 10 ticks per pixel: a column is 1000 frames.
        let clip = Clip { offset: 200, ..Clip::new("c", Color(0), SourceId(1), 400) };
        let map = WaveMap::of(&clip, 100., 0.1);
        assert_eq!(map.frames(0.), (20_000., 21_000.));
        assert_eq!(map.frames(50.), (25_000., 26_000.));
        let peaks = ramp(100_000);
        let mut cols = Vec::new();
        columns(&peaks, None, &clip, &map, 0., 40, &mut cols);
        assert_eq!(cols.len(), 40);
        for (i, (lo, hi)) in cols.iter().enumerate() {
            let from = 20_000. + 1000. * i as f64;
            // Peaks round outwards to whole buckets, and neighbours touch.
            assert!((frame_of(*lo, 100_000) - from).abs() <= SLACK, "column {i} starts at frame {}", frame_of(*lo, 100_000));
            assert!((frame_of(*hi, 100_000) - (from + 1000.)).abs() <= SLACK, "column {i} ends at frame {}", frame_of(*hi, 100_000));
        }
        // Only what is asked for is computed: a window far into the clip.
        columns(&peaks, None, &clip, &map, 300., 2, &mut cols);
        assert_eq!(cols.len(), 2);
        assert!((frame_of(cols[0].0, 100_000) - 50_000.).abs() <= SLACK);
        // Past the end of the file there is nothing.
        let long = Clip { offset: 900, ..clip.clone() };
        columns(&peaks, None, &long, &WaveMap::of(&long, 100., 0.1), 0., 20, &mut cols);
        assert!(cols[9].1 > 0.9 && cols[12] == (0., 0.), "{cols:?}");
    }

    #[test]
    fn columns_of_a_looping_clip_wrap_into_the_loop() {
        // The clip starts 100 ticks into its file and loops ticks 200..600.
        let clip = Clip { offset: 100, looping: Some(LoopRegion { start: 200, end: 600 }), ..Clip::new("c", Color(0), SourceId(1), 2000) };
        let map = WaveMap::of(&clip, 100., 0.1);
        // The first pass runs from the offset to the loop's end…
        assert_eq!(map.frames(0.), (10_000., 11_000.));
        assert_eq!(map.frames(490.), (59_000., 60_000.));
        // …then every pass starts at the loop's start.
        assert_eq!(map.frames(500.), (20_000., 21_000.));
        assert_eq!(map.frames(900.), (20_000., 21_000.));
        assert_eq!(map.frames(1290.), (59_000., 60_000.));
        // A column that straddles the wrap stops at the loop's end.
        assert_eq!(map.frames(495.), (59_500., 60_000.));
        let peaks = ramp(100_000);
        let mut cols = Vec::new();
        columns(&peaks, None, &clip, &map, 0., 130, &mut cols);
        let start = |i: usize| frame_of(cols[i].0, 100_000);
        assert!((start(0) - 10_000.).abs() <= SLACK && (start(49) - 59_000.).abs() <= SLACK);
        assert!(start(51) < 22_000. && start(91) < 22_000., "wrapped to the loop's start: {} {}", start(51), start(91));
        assert!(frame_of(cols[89].1, 100_000) > 59_000.);
        // Zoomed out until a pixel holds more than a whole loop: all of it.
        let far = WaveMap::of(&clip, 100., 0.001);
        assert_eq!(far.frames(5000.), (20_000., 60_000.));
    }

    #[test]
    fn columns_are_scaled_by_the_clip_gain_and_the_fades() {
        let square = Peaks::from_planar(&[(0..100_000).map(|i| if i % 2 == 0 { 1. } else { -1. }).collect()], 48_000);
        let mut clip = Clip::new("c", Color(0), SourceId(1), 1000);
        clip.fade_in = Fade { length: 500, curve: 0. };
        let map = WaveMap::of(&clip, 100., 0.1);
        let mut cols = Vec::new();
        columns(&square, None, &clip, &map, 0., 100, &mut cols);
        assert!(cols[0].1 < 0.02, "silent where the fade starts: {:?}", cols[0]);
        assert!((cols[25].1 - 0.51).abs() < 0.02 && (cols[25].0 + 0.51).abs() < 0.02, "half way up: {:?}", cols[25]);
        assert!((cols[75].1 - 1.).abs() < 1e-3);
        // -6 dB halves everything.
        clip.gain_db = -6.0206;
        columns(&square, None, &clip, &map, 0., 100, &mut cols);
        assert!((cols[75].1 - 0.5).abs() < 1e-3 && (cols[75].0 + 0.5).abs() < 1e-3);
    }
}
