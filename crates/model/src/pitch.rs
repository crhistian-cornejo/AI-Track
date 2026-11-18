//! The transpose envelope of an audio clip as a speed: how far into its file a
//! Re-Pitch (or unwarped) clip has read after some clip time. The engine reads
//! audio with it and the editing operations (split, trim, crop) place offsets
//! with it, so both agree on the source position of every song time.

use crate::project::{curve_shape, AutomationPoint, Clip, ClipEnv};
use crate::time::{Tick, PPQ};

/// Finest spacing of the area table, in ticks.
const PITCH_STEP: f64 = (PPQ / 16) as f64;
/// Most cells the area table gets (a very long envelope widens the step).
const PITCH_CELLS: f64 = 131_072.0;

/// Value of sorted `pts` at clip time `t` with a cursor `seg` (start it at 0
/// and pass it back, so consecutive samples do not search again). `None`
/// without points.
#[inline]
pub fn points_at(pts: &[AutomationPoint], t: f64, seg: &mut usize) -> Option<f32> {
    let first = pts.first()?;
    if t <= first.tick as f64 {
        return Some(first.value);
    }
    let mut i = (*seg).min(pts.len() - 1);
    while i + 1 < pts.len() && pts[i + 1].tick as f64 <= t {
        i += 1;
    }
    while i > 0 && pts[i].tick as f64 > t {
        i -= 1;
    }
    *seg = i;
    let a = pts[i];
    let Some(b) = pts.get(i + 1) else { return Some(a.value) };
    let x = (t - a.tick as f64) / b.tick.saturating_sub(a.tick).max(1) as f64;
    Some(a.value + (b.value - a.value) * curve_shape(x as f32, a.curve))
}

/// [`points_at`] without a cursor: a binary search, for random access.
#[inline]
pub fn points_search(pts: &[AutomationPoint], t: f64) -> Option<f32> {
    let first = pts.first()?;
    let mut seg = if t <= first.tick as f64 { 0 } else { pts.partition_point(|p| p.tick as f64 <= t).saturating_sub(1) };
    points_at(pts, t, &mut seg)
}

/// The sanitised transpose envelope of a clip with the table of its integral.
#[derive(Debug, Default)]
pub struct PitchCurve {
    /// Points of the enabled transpose envelope (semitones), sorted by tick.
    pub points: Box<[AutomationPoint]>,
    /// `∫ 2^(semitones/12) dt` from clip time 0 to `k × step`, in ticks.
    /// Empty without a transpose envelope.
    area: Box<[f64]>,
    step: f64,
}

impl PitchCurve {
    /// The clip's transpose envelope (curvatures and values held in range, as
    /// the engine reads it). Built on the control thread.
    pub fn of_clip(clip: &Clip) -> Self {
        let curve = |c: f32| if c.is_finite() { c.clamp(-1.0, 1.0) } else { 0.0 };
        let mut pts: Vec<AutomationPoint> = clip
            .envelope(ClipEnv::Pitch)
            .map(|lane| lane.points.iter().map(|p| AutomationPoint { tick: p.tick, value: ClipEnv::Pitch.clamp(p.value), curve: curve(p.curve) }).collect())
            .unwrap_or_default();
        pts.sort_by_key(|p| p.tick);
        let mut this = Self { points: pts.into_boxed_slice(), ..Self::default() };
        this.build();
        this
    }

    fn build(&mut self) {
        let Some(last) = self.points.last().map(|p| p.tick.max(0) as f64) else { return };
        let step = PITCH_STEP.max((last / PITCH_CELLS).ceil());
        let cells = (last / step).ceil() as usize + 1;
        let speed = |t: f64| 2f64.powf(points_search(&self.points, t).unwrap_or(0.0) as f64 / 12.0);
        let mut area = Vec::with_capacity(cells + 1);
        let mut sum = 0.0;
        area.push(sum);
        for k in 0..cells {
            let (a, b) = (k as f64 * step, (k + 1) as f64 * step);
            sum += step / 6.0 * (speed(a) + 4.0 * speed((a + b) / 2.0) + speed(b));
            area.push(sum);
        }
        self.area = area.into_boxed_slice();
        self.step = step;
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Transpose in semitones at clip time `t`.
    #[inline]
    pub fn semitones(&self, t: f64) -> f64 {
        points_search(&self.points, t).map_or(0.0, f64::from)
    }

    /// `∫ 2^(transpose/12) dτ` for τ in `0..t`: the clip ticks of the file
    /// read after `t` ticks of clip time at the envelope's speed (plain `t`
    /// without a transpose envelope). No allocation: a table lookup and a
    /// trapezoid inside one cell.
    #[inline]
    pub fn integral(&self, t: f64) -> f64 {
        let speed = |t: f64| 2f64.powf(self.semitones(t) / 12.0);
        let Some(&end) = self.area.last() else { return t };
        if t <= 0.0 {
            return t * speed(0.0);
        }
        let last = (self.area.len() - 1) as f64;
        let k = (t / self.step).floor().min(last);
        let a = k * self.step;
        let base = if k >= last { end } else { self.area[k as usize] };
        base + (t - a) * 0.5 * (speed(a) + speed(t))
    }

    /// Source ticks read over `delta` ticks of clip time at `rate` source
    /// ticks per clip tick: `rate × ∫₀^delta speed`.
    pub fn read_ticks(&self, delta: Tick, rate: f64) -> Tick {
        (rate * self.integral(delta as f64)).round() as Tick
    }
}
