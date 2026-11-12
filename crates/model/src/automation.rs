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

use crate::{AutomationLane, AutomationPoint, Tick};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AutomationTarget;

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
}
