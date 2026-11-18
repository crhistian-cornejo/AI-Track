//! Time-range edits of automation breakpoints, as in Live's automation
//! lanes: copy, delete, paste, duplicate and raise/lower a selected range.
//!
//! Every function takes the breakpoints of one lane (sorted by tick) and
//! returns the new list; the caller wraps it in a
//! [`Command::SetAutomation`](crate::Command::SetAutomation), so each edit is
//! one undo step. Ranges are half-open in spirit, `a..b`, with breakpoints
//! put on both edges so the envelope outside the range plays exactly as
//! before. Two breakpoints may share a tick: that is a vertical jump (the
//! later one holds from that tick on, as [`AutomationLane::value_at`] reads it).

use crate::{AutomationLane, AutomationPoint, AutomationTarget, Device, DeviceId, DeviceKind, Tick, Track, TrackKind};

fn value_at(points: &[AutomationPoint], tick: Tick) -> Option<f32> {
    AutomationLane { points: points.to_vec(), ..AutomationLane::new(crate::AutomationTarget::Volume) }.value_at(tick)
}

/// `points` with a breakpoint at `tick` that changes nothing: the value
/// there, and a curved segment cut in two keeps its exact shape (an
/// exponential segment cut at fraction `f` is two exponential segments
/// with the curvature shared out as `f` and `1 - f`). Unchanged when a
/// breakpoint is already there, or when there are no breakpoints.
pub fn split(points: &[AutomationPoint], tick: Tick) -> Vec<AutomationPoint> {
    let mut out = points.to_vec();
    let Some(value) = value_at(points, tick) else { return out };
    let i = out.partition_point(|p| p.tick < tick);
    if out.get(i).is_some_and(|p| p.tick == tick) {
        return out;
    }
    let mut curve = 0.0;
    if i > 0 && i < out.len() {
        let (prev, next) = (out[i - 1], out[i]);
        let f = (tick - prev.tick) as f32 / (next.tick - prev.tick).max(1) as f32;
        out[i - 1].curve = prev.curve * f;
        curve = prev.curve * (1.0 - f);
    }
    out.insert(i, AutomationPoint { tick, value, curve });
    out
}

/// What plays in `a..b`, as breakpoints from tick 0 (Copy): one on each
/// edge plus those between. Empty without breakpoints or for an empty range.
pub fn slice(points: &[AutomationPoint], a: Tick, b: Tick) -> Vec<AutomationPoint> {
    if b <= a || points.is_empty() {
        return Vec::new();
    }
    let cut = split(&split(points, a), b);
    // From the value right after `a` (the last breakpoint on it) to the one reached at `b` (the first on it).
    let first = cut.iter().rposition(|p| p.tick == a).unwrap_or(0);
    let last = cut.iter().position(|p| p.tick == b).unwrap_or(cut.len() - 1);
    let mut out: Vec<AutomationPoint> = cut[first..=last].iter().map(|p| AutomationPoint { tick: p.tick - a, ..*p }).collect();
    if let Some(end) = out.last_mut() {
        end.curve = 0.0;
    }
    out
}

/// Delete the automation in `a..b`: the breakpoints inside go and the
/// envelope runs straight from the value at `a` to the value at `b`, so
/// nothing outside the range changes.
pub fn clear(points: &[AutomationPoint], a: Tick, b: Tick) -> Vec<AutomationPoint> {
    if b <= a || points.is_empty() {
        return points.to_vec();
    }
    let cut = split(&split(points, a), b);
    // Keep the value reaching `a` and the one leaving `b`.
    let first_a = cut.iter().position(|p| p.tick == a);
    let last_b = cut.iter().rposition(|p| p.tick == b);
    let mut out: Vec<AutomationPoint> = cut
        .iter()
        .enumerate()
        .filter(|(i, p)| p.tick < a || p.tick > b || Some(*i) == first_a || Some(*i) == last_b)
        .map(|(_, p)| *p)
        .collect();
    if let Some(i) = out.iter().position(|p| p.tick == a) {
        out[i].curve = 0.0;
    }
    tidy(out)
}

/// Paste `clip` (breakpoints from tick 0, `span` ticks long) at `at`,
/// replacing what was in `at..at + span`. The envelope outside stays as it
/// was; where the pasted values differ from it at the edges it jumps.
pub fn paste(points: &[AutomationPoint], at: Tick, clip: &[AutomationPoint], span: Tick) -> Vec<AutomationPoint> {
    if clip.is_empty() || span <= 0 {
        return points.to_vec();
    }
    let end = at + span;
    let inner = clip.iter().filter(|p| p.tick <= span).map(|p| AutomationPoint { tick: p.tick + at, ..*p });
    if points.is_empty() {
        return tidy(inner.collect());
    }
    let cut = split(&split(points, at), end);
    let before = cut.iter().take_while(|p| p.tick < at).copied();
    // The value reaching `at` and the one leaving `end` from outside.
    let reach = cut.iter().find(|p| p.tick == at).copied();
    let leave = cut.iter().rev().find(|p| p.tick == end).copied();
    let after = cut.iter().skip_while(|p| p.tick <= end).copied();
    tidy(before.chain(reach).chain(inner).chain(leave).chain(after).collect())
}

/// Ctrl+D on a range: `a..b` again right after itself, over what was in
/// `b..2b - a`. Returns the new breakpoints and the range of the copy.
pub fn duplicate(points: &[AutomationPoint], a: Tick, b: Tick) -> (Vec<AutomationPoint>, (Tick, Tick)) {
    let span = b - a;
    (paste(points, b, &slice(points, a, b), span), (b, b + span))
}

/// Raise (positive `delta`) or lower the envelope inside `a..b` by `delta`
/// of the normalised range, each breakpoint clamped to `0..=1`. Breakpoints
/// on both edges keep the outside where it was, so the moved part jumps
/// there, as when dragging a selected segment in Live.
pub fn shift(points: &[AutomationPoint], a: Tick, b: Tick, delta: f32) -> Vec<AutomationPoint> {
    if b <= a || points.is_empty() || delta == 0.0 {
        return points.to_vec();
    }
    let moved: Vec<AutomationPoint> = slice(points, a, b).into_iter().map(|p| AutomationPoint { value: (p.value + delta).clamp(0.0, 1.0), ..p }).collect();
    paste(points, a, &moved, b - a)
}

// ───────────────────────────── Shapes and transforms ─────────────────────────────

/// A shape that can be written over a time selection of a lane (Live 12's
/// "Insert Shape": the waves on the top row, the ramps and the ADSR below).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Sine,
    Triangle,
    /// Rises from the bottom to the top, then jumps back (one period each).
    SawUp,
    /// Starts at the top and falls to the bottom, then jumps back.
    SawDown,
    Square,
    /// From the value the lane has at the start of the selection, fast at
    /// first, up to the top at its end.
    RampUp,
    /// From the value at the start, slowly at first, down to the bottom.
    RampDown,
    /// From the value at the start up to the top along an S.
    SCurveUp,
    /// From the value at the start down to the bottom along an S.
    SCurveDown,
    /// From the value at the start: attack to the top, decay to a sustain
    /// level, hold, release back to what the lane had at the end.
    Adsr,
}

impl Shape {
    pub const ALL: [Shape; 10] = [
        Shape::Sine,
        Shape::Triangle,
        Shape::SawUp,
        Shape::SawDown,
        Shape::Square,
        Shape::RampUp,
        Shape::RampDown,
        Shape::SCurveUp,
        Shape::SCurveDown,
        Shape::Adsr,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Sine => "Sine",
            Shape::Triangle => "Triangle",
            Shape::SawUp => "Sawtooth up",
            Shape::SawDown => "Sawtooth down",
            Shape::Square => "Square",
            Shape::RampUp => "Ramp up",
            Shape::RampDown => "Ramp down",
            Shape::SCurveUp => "S-curve up",
            Shape::SCurveDown => "S-curve down",
            Shape::Adsr => "ADSR",
        }
    }

    /// The waves repeat with the grid; the others span the whole selection.
    pub fn is_wave(self) -> bool {
        matches!(self, Shape::Sine | Shape::Triangle | Shape::SawUp | Shape::SawDown | Shape::Square)
    }

    /// One period of a wave as `(phase 0..=1, value)`; two entries on one phase are a jump.
    fn cycle(self) -> Vec<(f32, f32)> {
        match self {
            Shape::Sine => {
                const N: usize = 16;
                (0..=N).map(|k| (k as f32 / N as f32, 0.5 + 0.5 * (std::f32::consts::TAU * k as f32 / N as f32).sin())).collect()
            }
            Shape::Triangle => vec![(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)],
            Shape::SawUp => vec![(0.0, 0.0), (1.0, 1.0)],
            Shape::SawDown => vec![(0.0, 1.0), (1.0, 0.0)],
            Shape::Square => vec![(0.0, 1.0), (0.5, 1.0), (0.5, 0.0), (1.0, 0.0)],
            _ => Vec::new(),
        }
    }
}

/// Most periods a wave is cut into: a very fine grid over a long selection
/// writes fewer, longer cycles rather than a hundred thousand breakpoints.
const MAX_CYCLES: Tick = 256;

/// `shape` written over `a..b`, replacing what was there; the envelope
/// outside is untouched (it jumps at an edge where the shape does not meet
/// it). A wave repeats every `period` ticks from `a` (the grid step: one
/// cycle per beat at 1/4); a period of 0, or longer than the selection, is
/// one cycle over it. Ramps and the ADSR start from the value the lane has
/// at `a` (0.5 for an empty lane) and the ADSR returns to the value at `b`.
pub fn shape(points: &[AutomationPoint], a: Tick, b: Tick, shape: Shape, period: Tick) -> Vec<AutomationPoint> {
    let span = b - a;
    if span <= 0 {
        return points.to_vec();
    }
    let p = |tick: Tick, value: f32, curve: f32| AutomationPoint { tick, value: value.clamp(0.0, 1.0), curve };
    let clip: Vec<AutomationPoint> = if shape.is_wave() {
        let mut period = if period <= 0 || period > span { span } else { period };
        if span / period > MAX_CYCLES {
            period = (span + MAX_CYCLES - 1) / MAX_CYCLES;
        }
        let cycle = shape.cycle();
        let mut out: Vec<AutomationPoint> = Vec::new();
        let mut start = 0;
        while start < span {
            for &(phase, value) in &cycle {
                let tick = start + (phase * period as f32).round() as Tick;
                if tick >= span {
                    break;
                }
                out.push(p(tick, value, 0.0));
            }
            start += period;
        }
        // The value the wave has reached at the end of the selection.
        out.push(p(span, wave_end(&cycle, (span % period) as f32 / period as f32, span % period == 0), 0.0));
        tidy(out)
    } else {
        let from = value_at(points, a).unwrap_or(0.5);
        let to = value_at(points, b).unwrap_or(from);
        let f = |x: f32| (x * span as f32).round() as Tick;
        match shape {
            Shape::RampUp => vec![p(0, from, -0.6), p(span, 1.0, 0.0)],
            Shape::RampDown => vec![p(0, from, 0.6), p(span, 0.0, 0.0)],
            Shape::SCurveUp => vec![p(0, from, 0.6), p(f(0.5), (from + 1.0) * 0.5, -0.6), p(span, 1.0, 0.0)],
            Shape::SCurveDown => vec![p(0, from, 0.6), p(f(0.5), from * 0.5, -0.6), p(span, 0.0, 0.0)],
            _ => vec![p(0, from, -0.5), p(f(0.1), 1.0, 0.4), p(f(0.3), 0.6, 0.0), p(f(0.8), 0.6, 0.4), p(span, to, 0.0)],
        }
    };
    paste(points, a, &clip, span)
}

/// The value of one wave cycle at `phase`; at a whole period (`whole`) the value that ends the cycle.
fn wave_end(cycle: &[(f32, f32)], phase: f32, whole: bool) -> f32 {
    if whole {
        return cycle.last().map_or(0.0, |c| c.1);
    }
    let i = cycle.partition_point(|c| c.0 <= phase).clamp(1, cycle.len().saturating_sub(1).max(1));
    let (a, b) = (cycle[i - 1], cycle[i.min(cycle.len() - 1)]);
    if b.0 <= a.0 { a.1 } else { a.1 + (b.1 - a.1) * ((phase - a.0) / (b.0 - a.0)).clamp(0.0, 1.0) }
}

/// The lowest and highest value of the envelope inside `a..b` (the edges
/// included), the box the transform handles are drawn around.
pub fn extent(points: &[AutomationPoint], a: Tick, b: Tick) -> Option<(f32, f32)> {
    let inner = slice(points, a, b);
    let lo = inner.iter().map(|p| p.value).fold(f32::INFINITY, f32::min);
    let hi = inner.iter().map(|p| p.value).fold(f32::NEG_INFINITY, f32::max);
    (lo <= hi).then_some((lo, hi))
}

/// The envelope inside `a..b` with every value moved along `f`, clamped to `0..=1`.
fn map_values(points: &[AutomationPoint], a: Tick, b: Tick, f: impl Fn(&AutomationPoint) -> f32) -> Vec<AutomationPoint> {
    if b <= a || points.is_empty() {
        return points.to_vec();
    }
    let moved: Vec<AutomationPoint> = slice(points, a, b).into_iter().map(|p| AutomationPoint { value: f(&p).clamp(0.0, 1.0), ..p }).collect();
    paste(points, a, &moved, b - a)
}

/// Vertical stretch of `a..b` about `anchor` (a value; the bottom of the
/// selection's box when its top handle is dragged, the top for the bottom
/// handle, the middle for Alt): each value goes to `anchor + (v - anchor) * factor`.
pub fn scale(points: &[AutomationPoint], a: Tick, b: Tick, anchor: f32, factor: f32) -> Vec<AutomationPoint> {
    map_values(points, a, b, |p| anchor + (p.value - anchor) * factor)
}

/// Tilt of `a..b`: every value gains an offset that runs in a straight line
/// from `left` at `a` to `right` at `b` (a corner handle moves one end).
pub fn skew(points: &[AutomationPoint], a: Tick, b: Tick, left: f32, right: f32) -> Vec<AutomationPoint> {
    let span = (b - a).max(1) as f32;
    map_values(points, a, b, |p| p.value + left + (right - left) * ((p.tick) as f32 / span))
}

/// `a..b` moved and stretched in time to `na..nb`: what played there is
/// squeezed or spread over the new range, and what it leaves behind plays as
/// if it had never been there (a straight line between its edges); the new
/// range replaces what it lands on. A range of the same length is a plain move.
pub fn stretch(points: &[AutomationPoint], a: Tick, b: Tick, na: Tick, nb: Tick) -> Vec<AutomationPoint> {
    let (na, nb) = (na.max(0), nb.max(0));
    if b <= a || nb <= na || points.is_empty() || (na == a && nb == b) {
        return points.to_vec();
    }
    let f = (nb - na) as f64 / (b - a) as f64;
    let moved: Vec<AutomationPoint> = slice(points, a, b).into_iter().map(|p| AutomationPoint { tick: (p.tick as f64 * f).round() as Tick, ..p }).collect();
    paste(&clear(points, a, b), na, &moved, nb - na)
}

// ───────────────────────────── Simplify ─────────────────────────────

/// A segment's value at `tick`, as the lane reads it between two breakpoints.
fn segment_value(a: &AutomationPoint, b: &AutomationPoint, tick: Tick) -> f32 {
    let t = (tick - a.tick) as f32 / (b.tick - a.tick).max(1) as f32;
    a.value + (b.value - a.value) * crate::curve_shape(t, a.curve)
}

/// The most the straight line `points[i] → points[j]` is off the envelope
/// it replaces (at the dropped breakpoints and inside each curved segment),
/// and the breakpoint to keep to cut that error.
fn chord_error(points: &[AutomationPoint], i: usize, j: usize) -> (f32, usize) {
    let (a, b) = (&points[i], &points[j]);
    let line = |tick: Tick| a.value + (b.value - a.value) * ((tick - a.tick) as f32 / (b.tick - a.tick).max(1) as f32);
    let (mut worst, mut at) = (0.0f32, i + 1);
    for k in i..j {
        let (s, e) = (&points[k], &points[k + 1]);
        if k > i {
            let err = (s.value - line(s.tick)).abs();
            if err > worst {
                (worst, at) = (err, k);
            }
        }
        if s.curve.abs() > 1e-4 && e.tick > s.tick {
            for q in 1..4 {
                let tick = s.tick + (e.tick - s.tick) * q / 4;
                let err = (segment_value(s, e, tick) - line(tick)).abs();
                if err > worst {
                    // The curve bulges inside segment k..k+1: keep one of its ends.
                    (worst, at) = (err, if q <= 2 { k } else { k + 1 }.clamp(i + 1, j - 1));
                }
            }
        }
    }
    (worst, at)
}

/// Fewer breakpoints for the same envelope (Live's "Simplify Envelope"):
/// Ramer–Douglas–Peucker on the value axis, so the simplified line is never
/// much more than `tolerance` (of the normalised range) away from the
/// original; segments the line replaces are straight. Jumps (two
/// breakpoints on one tick) and the first and last breakpoint are kept.
pub fn simplify(points: &[AutomationPoint], tolerance: f32) -> Vec<AutomationPoint> {
    let n = points.len();
    if n < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    for k in 0..n - 1 {
        if points[k].tick == points[k + 1].tick {
            keep[k] = true;
            keep[k + 1] = true;
        }
    }
    // Each run between two kept breakpoints is simplified on its own.
    let anchors: Vec<usize> = (0..n).filter(|&k| keep[k]).collect();
    let mut stack: Vec<(usize, usize)> = anchors.windows(2).map(|w| (w[0], w[1])).collect();
    while let Some((i, j)) = stack.pop() {
        if j <= i + 1 {
            continue;
        }
        let (err, at) = chord_error(points, i, j);
        if err > tolerance {
            keep[at] = true;
            stack.push((i, at));
            stack.push((at, j));
        }
    }
    let kept: Vec<usize> = (0..n).filter(|&k| keep[k]).collect();
    kept.iter()
        .enumerate()
        .map(|(m, &k)| {
            // A line across dropped breakpoints is straight; neighbours keep their curve.
            let next = kept.get(m + 1).copied();
            AutomationPoint { curve: if next == Some(k + 1) { points[k].curve } else { 0.0 }, ..points[k] }
        })
        .collect()
}

/// [`simplify`] for the breakpoints inside `a..b` only (a selected range);
/// the envelope outside is unchanged.
pub fn simplify_range(points: &[AutomationPoint], a: Tick, b: Tick, tolerance: f32) -> Vec<AutomationPoint> {
    if b <= a || points.is_empty() {
        return points.to_vec();
    }
    let inner = simplify(&slice(points, a, b), tolerance);
    paste(points, a, &inner, b - a)
}

/// Drop breakpoints that change nothing: a jump of zero height (two on one
/// tick with the same value; the later keeps its curve), and any third or
/// later breakpoint on the same tick (only the first and last count).
fn tidy(points: Vec<AutomationPoint>) -> Vec<AutomationPoint> {
    let mut out: Vec<AutomationPoint> = Vec::with_capacity(points.len());
    for p in points {
        if let Some(last) = out.last_mut()
            && last.tick == p.tick
        {
            if (last.value - p.value).abs() < 1e-6 {
                *last = p;
                continue;
            }
            // A third one on the tick: the middle one is never heard.
            let n = out.len();
            if n >= 2 && out[n - 2].tick == p.tick {
                out[n - 1] = p;
                continue;
            }
        }
        out.push(p);
    }
    out
}

// ───────────────────────────── Targets ─────────────────────────────

/// Every device of a chain in order, at any rack depth (a rack comes before
/// what its chains hold), with how many racks it sits inside.
pub fn chain_devices(devices: &[Device]) -> Vec<(&Device, usize)> {
    fn walk<'a>(devices: &'a [Device], depth: usize, out: &mut Vec<(&'a Device, usize)>) {
        for d in devices {
            out.push((d, depth));
            if let DeviceKind::Rack(r) = &d.kind {
                for c in r.chains.iter().chain(&r.returns) {
                    walk(&c.devices, depth + 1, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(devices, 0, &mut out);
    out
}

/// One section of a track's automation chooser: the mixer (`device` is
/// `None`) or one device of its chain.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetGroup {
    pub device: Option<DeviceId>,
    /// How many racks the device sits inside (0 for the mixer).
    pub depth: usize,
    pub targets: Vec<AutomationTarget>,
}

/// What a track's lanes can automate, grouped as in Live's chooser: the
/// mixer first (Volume, Pan, Mute, one per send; Main adds Tempo), then each
/// device of the chain at any rack depth with its On switch, a rack's
/// macros, and the parameters `params` lists for it (the automatable ones,
/// in display order; the caller knows stock and plugin parameter lists).
pub fn targets(track: &Track, params: impl Fn(&Device) -> Vec<u32>) -> Vec<TargetGroup> {
    let mut mixer = Vec::new();
    if track.kind.carries_audio() {
        mixer.extend([AutomationTarget::Volume, AutomationTarget::Pan]);
        if track.kind != TrackKind::Main {
            mixer.push(AutomationTarget::Mute);
        }
        mixer.extend((0..track.sends.len()).map(|i| AutomationTarget::Send { index: i as u16 }));
    }
    if track.kind == TrackKind::Main {
        mixer.push(AutomationTarget::Tempo);
    }
    let mut out = vec![TargetGroup { device: None, depth: 0, targets: mixer }];
    for (d, depth) in chain_devices(&track.devices) {
        let mut targets = vec![AutomationTarget::DeviceEnabled { device: d.id }];
        if let DeviceKind::Rack(r) = &d.kind {
            targets.extend((0..r.macros.len()).map(|i| AutomationTarget::Macro { device: d.id, index: i as u8 }));
        } else {
            targets.extend(params(d).into_iter().map(|param| AutomationTarget::DeviceParam { device: d.id, param }));
        }
        out.push(TargetGroup { device: Some(d.id), depth, targets });
    }
    out
}

/// The device a target belongs to, if any.
pub fn target_device(target: AutomationTarget) -> Option<DeviceId> {
    match target {
        AutomationTarget::DeviceParam { device, .. } | AutomationTarget::DeviceEnabled { device } | AutomationTarget::Macro { device, .. } => Some(device),
        _ => None,
    }
}

/// `points` with a breakpoint written at `tick` (Live's automation
/// recording): what was between `from` (where this pass started, or the
/// last tick written) and `tick` is replaced, so a move overwrites the
/// envelope it passes over. The value is clamped to `0..=1`.
pub fn write(points: &[AutomationPoint], from: Tick, tick: Tick, value: f32) -> Vec<AutomationPoint> {
    let (a, b) = (from.min(tick), from.max(tick));
    let mut out: Vec<AutomationPoint> = points.iter().filter(|p| p.tick < a || p.tick > b || (p.tick == a && a < b)).copied().collect();
    let i = out.partition_point(|p| p.tick <= tick);
    out.insert(i, AutomationPoint { tick, value: value.clamp(0.0, 1.0), curve: 0.0 });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chain, Color, Rack, RackKind, Send, TrackId};

    fn pt(tick: Tick, value: f32) -> AutomationPoint {
        AutomationPoint { tick, value, curve: 0.0 }
    }

    fn at(points: &[AutomationPoint], tick: Tick) -> f32 {
        value_at(points, tick).unwrap()
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// A ramp 0 → 1 over 0..1000 then down to 0.5 at 2000.
    fn ramp() -> Vec<AutomationPoint> {
        vec![pt(0, 0.0), pt(1000, 1.0), pt(2000, 0.5)]
    }

    #[test]
    fn split_keeps_values_and_curved_shapes() {
        let curved = vec![AutomationPoint { tick: 0, value: 0.0, curve: 0.8 }, pt(1000, 1.0)];
        let cut = split(&curved, 300);
        assert_eq!(cut.len(), 3);
        for t in (0..=1000).step_by(50) {
            assert!(close(at(&curved, t), at(&cut, t)), "at {t}: {} vs {}", at(&curved, t), at(&cut, t));
        }
        // Outside the breakpoints: a flat point.
        let after = split(&ramp(), 3000);
        assert_eq!(after.last(), Some(&pt(3000, 0.5)));
        assert_eq!(split(&ramp(), 1000), ramp(), "nothing to add on a breakpoint");
        assert!(split(&[], 10).is_empty());
    }

    #[test]
    fn slice_interpolates_the_edges() {
        let s = slice(&ramp(), 500, 1500);
        assert_eq!(s, vec![pt(0, 0.5), pt(500, 1.0), pt(1000, 0.75)]);
        assert!(slice(&ramp(), 500, 500).is_empty());
        assert!(slice(&[], 0, 100).is_empty());
    }

    #[test]
    fn clear_runs_straight_between_the_edges_and_leaves_the_outside() {
        let c = clear(&ramp(), 500, 1500);
        assert_eq!(c, vec![pt(0, 0.0), pt(500, 0.5), pt(1500, 0.75), pt(2000, 0.5)]);
        for t in [0, 250, 499, 1500, 1750, 2000, 2500] {
            assert!(close(at(&ramp(), t), at(&c, t)), "outside unchanged at {t}");
        }
        assert!(close(at(&c, 1000), 0.625), "straight inside");
    }

    #[test]
    fn paste_overwrites_the_range_and_jumps_at_its_edges() {
        let clip = vec![pt(0, 0.2), pt(200, 0.2)];
        let p = paste(&ramp(), 1000, &clip, 200);
        assert_eq!(p, vec![pt(0, 0.0), pt(1000, 1.0), pt(1000, 0.2), pt(1200, 0.2), pt(1200, 0.9), pt(2000, 0.5)]);
        assert!(close(at(&p, 999), at(&ramp(), 999)));
        assert!(close(at(&p, 1100), 0.2));
        assert!(close(at(&p, 1200), at(&ramp(), 1200)), "the outside resumes at the end");
        assert!(close(at(&p, 1600), at(&ramp(), 1600)));
        // Into an empty lane: just the pasted points.
        assert_eq!(paste(&[], 100, &clip, 200), vec![pt(100, 0.2), pt(300, 0.2)]);
        // Pasting what is already there changes nothing heard and adds no jumps.
        let same = paste(&ramp(), 500, &slice(&ramp(), 500, 1500), 1000);
        assert_eq!(same, vec![pt(0, 0.0), pt(500, 0.5), pt(1000, 1.0), pt(1500, 0.75), pt(2000, 0.5)]);
    }

    #[test]
    fn duplicate_lays_the_range_right_after_itself() {
        let (d, range) = duplicate(&ramp(), 0, 1000);
        assert_eq!(range, (1000, 2000));
        for t in (0..1000).step_by(100) {
            assert!(close(at(&d, t + 1000), at(&ramp(), t)), "shifted copy at {t}");
            assert!(close(at(&d, t), at(&ramp(), t)), "original kept at {t}");
        }
        assert!(close(at(&d, 2000), 0.5), "what follows the copy is unchanged");
    }

    #[test]
    fn shift_moves_only_the_range_and_clamps() {
        let up = shift(&ramp(), 500, 1500, 0.3);
        assert!(close(at(&up, 499), at(&ramp(), 499)));
        assert!(close(at(&up, 500), 0.8));
        // Each breakpoint is clamped (1000 would be 1.3), the line runs between them.
        assert!(close(at(&up, 750), 0.9));
        assert!(close(at(&up, 1000), 1.0));
        assert!(close(at(&up, 1499), (at(&ramp(), 1499) + 0.3).min(1.0)));
        assert!(close(at(&up, 1500), at(&ramp(), 1500)), "the outside resumes at the end");
        assert!(close(at(&up, 1800), at(&ramp(), 1800)));
        let down = shift(&ramp(), 0, 2000, -2.0);
        assert!(down.iter().filter(|p| p.tick < 2000).all(|p| p.value == 0.0), "clamped at the bottom");
        assert_eq!(shift(&ramp(), 500, 1500, 0.0), ramp());
        // Undo-friendly: the lane's command can restore the original.
        let lane = AutomationLane { points: up, ..AutomationLane::new(AutomationTarget::Pan) };
        assert!(lane.points.windows(2).all(|w| w[0].tick <= w[1].tick), "still sorted");
    }

    fn dev(id: u64) -> Device {
        Device::stock(DeviceId(id), "fx.eq", format!("D{id}"))
    }

    #[test]
    fn targets_list_the_mixer_then_every_device_inside_racks() {
        let mut t = Track::new(TrackId(1), TrackKind::Audio, "a", Color(0));
        t.sends.push(Send { target: TrackId(9), level_db: 0.0, pre_fader: false });
        let mut inner = Rack::new(RackKind::Audio);
        let mut chain = Chain::new("c", Color(0));
        chain.devices.push(dev(12));
        inner.chains.push(chain);
        let mut rack = dev(11);
        rack.kind = DeviceKind::Rack(inner);
        t.devices = vec![dev(10), rack];
        let groups = targets(&t, |d| if d.id == DeviceId(12) { vec![3, 4] } else { vec![0] });
        assert_eq!(groups[0].targets, vec![AutomationTarget::Volume, AutomationTarget::Pan, AutomationTarget::Mute, AutomationTarget::Send { index: 0 }]);
        let devices: Vec<(Option<DeviceId>, usize)> = groups.iter().map(|g| (g.device, g.depth)).collect();
        assert_eq!(devices, vec![(None, 0), (Some(DeviceId(10)), 0), (Some(DeviceId(11)), 0), (Some(DeviceId(12)), 1)]);
        let rack = &groups[2].targets;
        assert_eq!(rack[0], AutomationTarget::DeviceEnabled { device: DeviceId(11) });
        assert_eq!(rack.len(), 1 + crate::MACRO_COUNT, "a rack offers its macros");
        assert_eq!(groups[3].targets[1..], [AutomationTarget::DeviceParam { device: DeviceId(12), param: 3 }, AutomationTarget::DeviceParam { device: DeviceId(12), param: 4 }]);
        assert_eq!(target_device(groups[3].targets[2]), Some(DeviceId(12)));
        // Main: no Mute, but the tempo lane.
        let main = Track::new(TrackId(2), TrackKind::Main, "Main", Color(0));
        assert_eq!(targets(&main, |_| Vec::new())[0].targets, vec![AutomationTarget::Volume, AutomationTarget::Pan, AutomationTarget::Tempo]);
    }

    #[test]
    fn writing_overwrites_what_the_move_passes_over() {
        let p = write(&ramp(), 0, 0, 0.3);
        assert_eq!(p[0], pt(0, 0.3), "replaces the breakpoint it lands on");
        // A pass from 500 to 1500: the breakpoint at 1000 goes.
        let mut pts = write(&ramp(), 500, 500, 0.2);
        pts = write(&pts, 500, 1500, 0.4);
        assert_eq!(pts, vec![pt(0, 0.0), pt(500, 0.2), pt(1500, 0.4), pt(2000, 0.5)]);
        // Into an empty lane.
        assert_eq!(write(&[], 0, 100, 1.5), vec![pt(100, 1.0)]);
    }

    const BEAT: Tick = crate::PPQ;

    /// The largest difference between two envelopes at any tick of `0..=end`.
    fn max_error(a: &[AutomationPoint], b: &[AutomationPoint], end: Tick) -> f32 {
        (0..=end).map(|t| (at(a, t) - at(b, t)).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn a_sine_follows_the_grid_and_leaves_the_outside_alone() {
        let base = vec![pt(0, 0.2), pt(16 * BEAT, 0.2)];
        let out = shape(&base, 4 * BEAT, 8 * BEAT, Shape::Sine, BEAT);
        // One cycle per beat: mid, top, mid, bottom, mid.
        assert!(close(at(&out, 4 * BEAT), 0.5));
        assert!(close(at(&out, 4 * BEAT + BEAT / 4), 1.0));
        assert!(close(at(&out, 4 * BEAT + BEAT / 2), 0.5));
        assert!(close(at(&out, 4 * BEAT + 3 * BEAT / 4), 0.0));
        assert!(close(at(&out, 7 * BEAT + BEAT / 4), 1.0), "the last cycle repeats it");
        assert!(out.iter().all(|p| (0.0..=1.0).contains(&p.value)));
        assert!(close(at(&out, 2 * BEAT), 0.2) && close(at(&out, 12 * BEAT), 0.2), "outside is unchanged");
        assert!(out.len() < 100, "{} breakpoints", out.len());
    }

    #[test]
    fn a_partial_last_cycle_stops_on_the_wave_at_the_end() {
        let out = shape(&[], 0, BEAT + BEAT / 4, Shape::Triangle, BEAT);
        assert!(close(at(&out, BEAT + BEAT / 4), 0.5), "a quarter into the second cycle is half way up");
        assert_eq!(out.last().map(|p| p.tick), Some(BEAT + BEAT / 4));
    }

    #[test]
    fn square_and_saw_jump_at_the_cycle_edges() {
        let sq = shape(&[], 0, 2 * BEAT, Shape::Square, BEAT);
        assert!(close(at(&sq, BEAT / 4), 1.0) && close(at(&sq, 3 * BEAT / 4), 0.0));
        assert!(close(at(&sq, BEAT), 1.0), "the second cycle starts high, with a jump");
        assert!(sq.windows(2).any(|w| w[0].tick == w[1].tick), "vertical edges are two breakpoints on one tick");
        let saw = shape(&[], 0, 2 * BEAT, Shape::SawUp, BEAT);
        assert!(close(at(&saw, BEAT / 2), 0.5) && at(&saw, BEAT - 1) > 0.99);
        assert!(close(at(&saw, BEAT), 0.0), "and drops back");
        let down = shape(&[], 0, BEAT, Shape::SawDown, BEAT);
        assert!(close(at(&down, 0), 1.0) && close(at(&down, BEAT), 0.0));
    }

    #[test]
    fn ramps_and_the_adsr_start_from_the_existing_value() {
        let base = vec![pt(0, 0.3), pt(8 * BEAT, 0.3)];
        for shape_ in [Shape::RampUp, Shape::RampDown, Shape::SCurveUp, Shape::SCurveDown, Shape::Adsr] {
            let out = shape(&base, 2 * BEAT, 4 * BEAT, shape_, BEAT);
            assert!(close(at(&out, 2 * BEAT), 0.3), "{shape_:?} starts where the lane was");
        }
        assert!(at(&shape(&base, 2 * BEAT, 4 * BEAT, Shape::RampUp, 0), 4 * BEAT - 1) > 0.99);
        assert!(at(&shape(&base, 2 * BEAT, 4 * BEAT, Shape::SCurveDown, 0), 4 * BEAT - 1) < 0.01);
        // The ADSR peaks at the top and comes back to what was at the end.
        let adsr = shape(&base, 2 * BEAT, 4 * BEAT, Shape::Adsr, 0);
        assert!(adsr.iter().any(|p| close(p.value, 1.0)));
        assert!(close(at(&adsr, 4 * BEAT), 0.3));
        // An empty lane works too.
        assert!(!shape(&[], 0, BEAT, Shape::Adsr, 0).is_empty());
        assert_eq!(shape(&base, 5, 5, Shape::Sine, 0), base, "an empty range changes nothing");
    }

    #[test]
    fn a_fine_grid_over_a_long_range_stays_bounded() {
        let out = shape(&[], 0, 1000 * BEAT, Shape::Sine, 1);
        assert!(out.len() < 256 * 20, "{} breakpoints", out.len());
    }

    #[test]
    fn scale_stretches_about_the_anchor() {
        let base = vec![pt(0, 0.4), pt(BEAT, 0.8), pt(2 * BEAT, 0.4), pt(4 * BEAT, 0.9)];
        // About the bottom of the box (0.4): the top 0.8 doubles its height.
        let out = scale(&base, 0, 2 * BEAT, 0.4, 2.0);
        assert!(close(at(&out, BEAT), 1.0) && (at(&out, 1) - 0.4).abs() < 0.01);
        assert!(close(at(&out, 3 * BEAT), at(&base, 3 * BEAT)), "outside is unchanged");
        // About the middle, halved; values stay clamped.
        let flat = scale(&base, 0, 2 * BEAT, 0.6, 0.5);
        assert!(close(at(&flat, BEAT), 0.7) && (at(&flat, 1) - 0.5).abs() < 0.01, "{flat:?}");
        assert!(scale(&base, 0, 2 * BEAT, 0.0, 10.0).iter().all(|p| p.value <= 1.0));
        assert_eq!(extent(&base, 0, 2 * BEAT), Some((0.4, 0.8)));
    }

    #[test]
    fn skew_tilts_the_range_from_one_edge() {
        let base = vec![pt(0, 0.5), pt(4 * BEAT, 0.5)];
        let out = skew(&base, BEAT, 3 * BEAT, 0.0, 0.2);
        assert!(close(at(&out, BEAT), 0.5) && close(at(&out, 2 * BEAT), 0.6) && (at(&out, 3 * BEAT - 1) - 0.7).abs() < 0.01);
        assert!(close(at(&out, 0), 0.5) && close(at(&out, 4 * BEAT), 0.5), "the outside does not tilt");
    }

    #[test]
    fn stretching_in_time_rescales_the_range_and_a_move_keeps_its_length() {
        let base = vec![pt(0, 0.0), pt(2 * BEAT, 0.0), pt(3 * BEAT, 1.0), pt(4 * BEAT, 0.0), pt(8 * BEAT, 0.0)];
        // The peak at 3 beats, in a range 2..4 doubled to 2..6, moves to 4.
        let out = stretch(&base, 2 * BEAT, 4 * BEAT, 2 * BEAT, 6 * BEAT);
        assert!(close(at(&out, 4 * BEAT), 1.0), "{out:?}");
        assert!(close(at(&out, 7 * BEAT), 0.0));
        // Squeezed to a half from the right edge.
        let squeezed = stretch(&base, 2 * BEAT, 4 * BEAT, 2 * BEAT, 3 * BEAT);
        assert!(close(at(&squeezed, 2 * BEAT + BEAT / 2), 1.0));
        // Moved by two beats: the peak follows and the old place is flat.
        let moved = stretch(&base, 2 * BEAT, 4 * BEAT, 4 * BEAT, 6 * BEAT);
        assert!(close(at(&moved, 5 * BEAT), 1.0) && close(at(&moved, 3 * BEAT), 0.0));
        assert_eq!(stretch(&base, 4 * BEAT, 2 * BEAT, 0, 1), base);
    }

    fn dense_sine(n: Tick) -> Vec<AutomationPoint> {
        (0..=n).map(|i| pt(i * 10, 0.5 + 0.4 * (i as f32 * 0.05).sin())).collect()
    }

    #[test]
    fn simplify_cuts_points_and_keeps_the_curve_within_tolerance() {
        let pts = dense_sine(400);
        for tol in [0.002, 0.005, 0.02] {
            let out = simplify(&pts, tol);
            assert!(out.len() < pts.len() / 2, "tol {tol}: {} of {}", out.len(), pts.len());
            let err = max_error(&pts, &out, 4000);
            assert!(err <= tol + 1e-4, "tol {tol}: error {err}");
            assert_eq!((out[0], out[out.len() - 1]), (pts[0], pts[pts.len() - 1]), "the ends stay");
        }
        // A bigger tolerance never keeps more.
        assert!(simplify(&pts, 0.05).len() <= simplify(&pts, 0.005).len());
    }

    #[test]
    fn simplify_collapses_a_line_and_keeps_jumps_and_noise_free_corners() {
        let line: Vec<AutomationPoint> = (0..50).map(|i| pt(i * 100, i as f32 / 49.0)).collect();
        assert_eq!(simplify(&line, 0.001).len(), 2);
        let step = vec![pt(0, 0.2), pt(100, 0.2), pt(100, 0.8), pt(200, 0.8), pt(300, 0.8)];
        let out = simplify(&step, 0.001);
        assert_eq!(out, vec![pt(0, 0.2), pt(100, 0.2), pt(100, 0.8), pt(300, 0.8)], "the jump survives");
        assert_eq!(simplify(&[pt(0, 0.1), pt(5, 0.9)], 0.5).len(), 2);
        assert!(simplify(&[], 0.1).is_empty());
    }

    #[test]
    fn simplify_counts_the_curve_of_a_segment_it_replaces() {
        // One strongly curved segment between two points that a straight line would flatten.
        let curved = vec![AutomationPoint { tick: 0, value: 0.0, curve: 1.0 }, pt(1000, 1.0), pt(2000, 1.0)];
        let out = simplify(&curved, 0.01);
        assert!(max_error(&curved, &out, 2000) <= 0.011);
        assert_eq!(out[0].curve, 1.0, "kept neighbours keep their curve");
        // A three-point curved run that is nearly straight may be dropped, never made worse than the tolerance.
        let wavy: Vec<AutomationPoint> = (0..20).map(|i| AutomationPoint { tick: i * 50, value: 0.5 + 0.2 * (i as f32).sin(), curve: 0.3 }).collect();
        let out = simplify(&wavy, 0.03);
        assert!(max_error(&wavy, &out, 950) <= 0.03 + 1e-3, "{}", max_error(&wavy, &out, 950));
    }

    #[test]
    fn simplify_range_leaves_the_outside_alone() {
        let pts = dense_sine(400);
        let out = simplify_range(&pts, 1000, 3000, 0.01);
        assert!(out.len() < pts.len());
        assert!(max_error(&pts, &out, 4000) <= 0.011);
        for p in pts.iter().filter(|p| p.tick < 1000 || p.tick > 3000) {
            assert!(out.contains(p), "{p:?} outside the range is kept");
        }
    }
}
