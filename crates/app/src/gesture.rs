//! Trackpad and mouse-wheel navigation shared by the dense views
//! (arrangement, piano roll, sample editor, steps, mixer): wheel → pan
//! deltas, Cmd/Ctrl + wheel → zoom, pinch → zoom, and zoom anchored on the
//! pointer. The maths is pure and tested here; the views wire it up.
//!
//! # Subscribing
//!
//! GPUI elements expose both listeners directly (`InteractiveElement`), so
//! no window-level hook is needed:
//!
//! ```ignore
//! div()
//!     .id("lanes")                         // any interactive element
//!     .on_scroll_wheel(cx.listener(|ws, e: &ScrollWheelEvent, _, cx| {
//!         match gesture::wheel(e) {
//!             Wheel::Pan { dx, dy } => { /* scroll by (dx, dy) */ }
//!             Wheel::Zoom { factor } => { /* zoom_anchored(…, factor, …) */ }
//!         }
//!         cx.notify();
//!     }))
//!     .on_pinch(cx.listener(|ws, e: &PinchEvent, _, cx| {
//!         let factor = gesture::pinch_factor(e);
//!         // anchor at e.position, like Wheel::Zoom
//!         cx.notify();
//!     }))
//! ```
//!
//! `on_pinch` fires in the bubble phase only while the element's hitbox is
//! hovered (the pinch centre is over it); `capture_pinch` sees every pinch
//! in the capture phase. A `canvas` has no listeners of its own: put the
//! handlers on the `div` that holds it. Pinches arrive as
//! [`gpui::TouchPhase`] `Started` / `Moved` / `Ended`; each event carries
//! an incremental `delta`, so apply [`pinch_factor`] per event.
#![allow(dead_code)] // Wired into the views by the navigation work.

use gpui::{PinchEvent, ScrollDelta, ScrollWheelEvent};

/// Pixels per line for wheels that report lines (most mice on Windows).
pub const LINE_PX: f32 = 40.;
/// How strongly the zoom follows the wheel: `exp(dy / ZOOM_PX)`.
const ZOOM_PX: f32 = 400.;

/// What a wheel event asks a view to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wheel {
    /// Move the content by this many pixels (positive = content moves
    /// right/down, so subtract from a scroll offset).
    Pan { dx: f32, dy: f32 },
    /// Multiply the scale by `factor` (> 1 zooms in).
    Zoom { factor: f32 },
}

/// Pixel deltas of a wheel event on both axes. Line deltas use `line_px`.
/// A line wheel (a mouse) with Shift scrolls sideways, as everywhere.
pub fn pan_delta(delta: ScrollDelta, shift: bool, line_px: f32) -> (f32, f32) {
    match delta {
        ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
        ScrollDelta::Lines(l) => {
            let (dx, dy) = (l.x * line_px, l.y * line_px);
            if shift && dx == 0. { (dy, 0.) } else { (dx, dy) }
        }
    }
}

/// The zoom factor of a Cmd/Ctrl + wheel step from its vertical delta.
pub fn wheel_zoom_factor(dy: f32) -> f32 {
    (dy / ZOOM_PX).exp()
}

/// Whether the wheel zooms: Cmd on macOS, Ctrl on Windows (and Ctrl on
/// macOS, which some mice send for their zoom wheel).
pub fn is_zoom(e: &ScrollWheelEvent) -> bool {
    e.modifiers.secondary() || e.modifiers.control
}

/// Classify a wheel event as pan or zoom.
pub fn wheel(e: &ScrollWheelEvent) -> Wheel {
    let (dx, dy) = pan_delta(e.delta, e.modifiers.shift, LINE_PX);
    if is_zoom(e) {
        // Some trackpads put a pinch-like scroll on x; take the larger axis.
        let d = if dx.abs() > dy.abs() { dx } else { dy };
        Wheel::Zoom { factor: wheel_zoom_factor(d) }
    } else {
        Wheel::Pan { dx, dy }
    }
}

/// The zoom factor of one pinch event (its `delta` is incremental, 0.1 =
/// 10 % bigger). Never zero or negative.
pub fn pinch_factor(e: &PinchEvent) -> f32 {
    pinch_delta_factor(e.delta)
}

pub fn pinch_delta_factor(delta: f32) -> f32 {
    (1. + delta).max(0.05)
}

/// Zoom about a point. `offset` is the scroll offset in content pixels at
/// `scale` (content px = units × scale), `anchor_px` the pointer's distance
/// from the view's leading edge. Returns the new `(offset, scale)` with the
/// content under the pointer left where it was, the scale clamped to
/// `[min, max]` and the offset kept ≥ 0.
pub fn zoom_anchored(offset: f32, scale: f32, anchor_px: f32, factor: f32, min: f32, max: f32) -> (f32, f32) {
    let scale_new = (scale * factor).clamp(min, max);
    if scale <= 0. {
        return (offset.max(0.), scale_new);
    }
    let units_at = (offset + anchor_px) / scale;
    ((units_at * scale_new - anchor_px).max(0.), scale_new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn pixels_pass_through_and_lines_scale() {
        assert_eq!(pan_delta(ScrollDelta::Pixels(point(px(3.), px(-7.))), false, 40.), (3., -7.));
        assert_eq!(pan_delta(ScrollDelta::Lines(point(0., 2.)), false, 40.), (0., 80.));
    }

    #[test]
    fn shift_swaps_axes_for_line_wheels_only() {
        assert_eq!(pan_delta(ScrollDelta::Lines(point(0., 1.)), true, 40.), (40., 0.));
        // A trackpad already reports both axes: leave it alone.
        assert_eq!(pan_delta(ScrollDelta::Pixels(point(px(0.), px(5.))), true, 40.), (0., 5.));
    }

    #[test]
    fn wheel_zoom_is_symmetric() {
        let f = wheel_zoom_factor(100.) * wheel_zoom_factor(-100.);
        assert!((f - 1.).abs() < 1e-6);
        assert!(wheel_zoom_factor(10.) > 1.);
    }

    #[test]
    fn pinch_never_inverts() {
        assert!((pinch_delta_factor(0.1) - 1.1).abs() < 1e-6);
        assert!(pinch_delta_factor(-5.) > 0.);
    }

    #[test]
    fn zoom_keeps_the_anchor_fixed() {
        let (offset, scale, anchor) = (200., 10., 150.);
        let before = (offset + anchor) / scale;
        let (o, s) = zoom_anchored(offset, scale, anchor, 2., 1., 100.);
        assert_eq!(s, 20.);
        assert!(((o + anchor) / s - before).abs() < 1e-4);
    }

    #[test]
    fn zoom_clamps_scale_and_offset() {
        let (_, s) = zoom_anchored(0., 10., 0., 100., 1., 50.);
        assert_eq!(s, 50.);
        let (o, s) = zoom_anchored(10., 10., 5., 0.01, 1., 50.);
        assert_eq!(s, 1.);
        assert!(o >= 0.);
    }
}
