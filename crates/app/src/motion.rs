//! Shared motion: durations, curves and the few entrances several surfaces
//! use, so the whole app moves as one thing (adapted from monoBOT's
//! `motion.rs`, same owner, MIT).
//!
//! Nothing here runs a permanent tick. GPUI animations and springs request
//! frames only while they move, and [`Tween`] reports `done` so its owner
//! stops asking for frames. Under reduced motion every entrance renders its
//! final state immediately.

use gpui::{
    Animation, AnimationElement, AnimationExt, App, ElementId, IntoElement, SpringAnimation, SpringConfig, Styled,
    px,
};
use std::time::{Duration, Instant};

/// A CSS cubic-bezier: `(x1, y1, x2, y2)`.
pub type Curve = (f32, f32, f32, f32);

/// Fast out and a long settle: whatever arrives or changes shape.
pub const EMPHASIZED: Curve = (0.22, 1., 0.36, 1.);
/// CSS `ease-out`: fades and short moves.
pub const EASE_OUT: Curve = (0., 0., 0.58, 1.);
/// Menus and flyouts opening.
pub const DECELERATE: Curve = (0.16, 1., 0.3, 1.);
/// Turns that start and end at rest.
pub const STANDARD: Curve = (0.4, 0., 0.2, 1.);

/// Hover feedback, toggles, reorders.
pub const FAST: Duration = Duration::from_millis(160);
/// Menus, tabs, fades.
pub const QUICK: Duration = Duration::from_millis(200);
/// Panels and view switches.
pub const NORMAL: Duration = Duration::from_millis(240);

/// The spring panels slide on: critically damped, a quick start and a quiet finish.
pub const PANEL_SPRING: SpringConfig = SpringConfig::new(420., 41., 1.);
/// The spring selection indicators glide on (segmented controls, tab underlines).
pub const INDICATOR_SPRING: SpringConfig = SpringConfig::new(520., 46., 1.);

/// `t` (0 to 1) along a cubic-bezier.
pub fn bezier(t: f32, curve: Curve) -> f32 {
    let (x1, y1, x2, y2) = curve;
    if t <= 0. {
        return 0.;
    }
    if t >= 1. {
        return 1.;
    }
    let f = |s: f32, a: f32, b: f32| 3. * (1. - s) * (1. - s) * s * a + 3. * (1. - s) * s * s * b + s * s * s;
    let (mut lo, mut hi) = (0., 1.);
    for _ in 0..14 {
        let mid = (lo + hi) * 0.5;
        if f(mid, x1, x2) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    f((lo + hi) * 0.5, y1, y2)
}

pub fn ease(curve: Curve) -> impl Fn(f32) -> f32 + 'static {
    move |t| bezier(t, curve)
}

pub fn cubic_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

/// A spring that moves an `f32` to `to`, keeping velocity when retargeted.
pub fn spring(config: SpringConfig, to: f32) -> SpringAnimation<f32> {
    SpringAnimation::new(config).to(to).with_epsilon(0.002)
}

/// Fades `element` in while it rises `travel` px into place.
pub fn rise_in<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    element: E,
    travel: f32,
) -> AnimationElement<E> {
    element.with_animation(id, Animation::new(QUICK).with_easing(ease(DECELERATE)), move |el, t| {
        el.relative().top(px(travel * (1. - t))).opacity(t)
    })
}

/// Fades `element` in where it stands.
pub fn fade_in<E: Styled + IntoElement + 'static>(id: impl Into<ElementId>, element: E) -> AnimationElement<E> {
    element.with_animation(id, Animation::new(QUICK).with_easing(cubic_out), |el, t| el.opacity(t))
}

/// Slides `element` in from `dx` px to the side (view switches).
pub fn slide_in<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    element: E,
    dx: f32,
) -> AnimationElement<E> {
    element.with_animation(id, Animation::new(NORMAL).with_easing(ease(EMPHASIZED)), move |el, t| {
        el.relative().left(px(dx * (1. - t))).opacity(0.25 + 0.75 * t)
    })
}

/// A time-based value that owns its own clock; the owner keeps requesting
/// frames while [`Tween::sample`] says it is still moving.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
    curve: Curve,
}

impl Tween {
    pub fn settled(value: f32) -> Self {
        Self { from: value, to: value, start: Instant::now(), duration: Duration::ZERO, curve: EASE_OUT }
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    /// Retarget from wherever the value is now.
    pub fn retarget(&mut self, to: f32, duration: Duration, curve: Curve, now: Instant) {
        let (current, _) = self.sample(now);
        *self = Self { from: current, to, start: now, duration, curve };
    }

    /// `(value, done)`.
    pub fn sample(&self, now: Instant) -> (f32, bool) {
        if self.duration.is_zero() {
            return (self.to, true);
        }
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32();
        if t >= 1. {
            return (self.to, true);
        }
        (self.from + (self.to - self.from) * bezier(t, self.curve), false)
    }
}

/// Whether the platform asks for reduced motion.
pub fn reduced(cx: &App) -> bool {
    cx.reduce_motion()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_start_at_rest_and_end_in_place() {
        for curve in [EMPHASIZED, EASE_OUT, DECELERATE, STANDARD] {
            assert_eq!(bezier(0., curve), 0.);
            assert_eq!(bezier(1., curve), 1.);
            let mut last = 0.;
            for step in 1..=20 {
                let at = bezier(step as f32 / 20., curve);
                assert!(at >= last - 1e-4);
                last = at;
            }
        }
    }

    #[test]
    fn tween_settles_and_retargets_from_current_value() {
        let now = Instant::now();
        let mut t = Tween::settled(0.);
        assert_eq!(t.sample(now), (0., true));
        t.retarget(1., Duration::from_millis(100), EASE_OUT, now);
        let (mid, done) = t.sample(now + Duration::from_millis(50));
        assert!(!done && mid > 0. && mid < 1.);
        assert_eq!(t.sample(now + Duration::from_millis(200)), (1., true));
    }

    #[test]
    fn timings_stay_in_order() {
        assert!(FAST < QUICK && QUICK < NORMAL);
    }
}
