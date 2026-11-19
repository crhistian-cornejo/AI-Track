//! The macOS window buttons and the bar drawn around them.
//!
//! The close, minimise and zoom buttons keep their native size whatever the
//! interface zoom is, while the top bar grows and shrinks with it. So the bar
//! asks AppKit where the buttons really end and how tall they are: the room it
//! leaves for them and their vertical centre both follow the zoom and the
//! spacing of the macOS version in use.

use gpui::Window;

/// Where the buttons sit from the window's left edge (points).
#[cfg(target_os = "macos")]
const LEFT: f32 = 14.;
/// The buttons' right edge and height when AppKit cannot be asked (points).
const FALLBACK: (f32, f32) = (74., 14.);
/// Clear space between the last button and the bar's first control (points).
const GAP: f32 = 6.;

/// Right edge and height of the row of buttons, in window points.
#[cfg(target_os = "macos")]
fn measure(window: &Window) -> Option<(f32, f32)> {
    use objc2::runtime::AnyObject;
    use objc2::{Encode, Encoding, msg_send};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]), Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING])]);
    }
    /// `NSWindowZoomButton`: the last of the three.
    const ZOOM_BUTTON: usize = 2;

    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    // SAFETY: GPUI's AppKit handle is its live content view; the messages are
    // plain getters sent on the main thread, where rendering runs.
    unsafe {
        let view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return None;
        }
        let button: *const AnyObject = msg_send![ns_window, standardWindowButton: ZOOM_BUTTON];
        if button.is_null() {
            return None;
        }
        let frame: Rect = msg_send![button, frame];
        let (end, h) = ((frame.x + frame.w) as f32, frame.h as f32);
        (end.is_finite() && h.is_finite() && end > 0. && h > 0.).then_some((end, h))
    }
}

#[cfg(not(target_os = "macos"))]
fn measure(_: &Window) -> Option<(f32, f32)> {
    None
}

/// Width the bar keeps free at its left for buttons ending at `end` points, in
/// layout units (what `px()` takes under `zoom`), on the 4 px grid.
pub fn inset_for(end: f32, zoom: f32) -> f32 {
    (((end + GAP) / zoom.max(0.25)) / 4.).ceil() * 4.
}

/// Width the top bar keeps free at its left. Full screen has no buttons.
pub fn inset(window: &Window) -> f32 {
    if !cfg!(target_os = "macos") || window.is_fullscreen() {
        return crate::theme::SP2;
    }
    inset_for(measure(window).unwrap_or(FALLBACK).0, window.content_zoom())
}

/// Distance from the window's top to the buttons so they sit at the middle of
/// a bar `bar_h` layout units tall under `zoom` (points).
pub fn top_for(bar_h: f32, zoom: f32, button_h: f32) -> f32 {
    ((bar_h * zoom - button_h) / 2.).max(0.)
}

/// Centre the buttons in a bar `bar_h` layout units tall. Call when the zoom
/// or the density changes.
#[cfg(target_os = "macos")]
pub fn centre(window: &Window, bar_h: f32) {
    let button_h = measure(window).unwrap_or(FALLBACK).1;
    window.set_traffic_light_position(gpui::point(gpui::px(LEFT), gpui::px(top_for(bar_h, window.content_zoom(), button_h))));
}

#[cfg(not(target_os = "macos"))]
pub fn centre(_: &Window, _: f32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inset_keeps_clear_of_the_buttons_at_every_zoom() {
        for zoom in [0.8, 0.9, 1., 1.25, 1.5] {
            let inset = inset_for(FALLBACK.0, zoom);
            assert_eq!(inset % 4., 0.);
            // What the inset measures on screen covers the buttons and the gap.
            assert!(inset * zoom >= FALLBACK.0 + GAP - 0.01, "{zoom}: {inset}");
            assert!(inset * zoom < FALLBACK.0 + GAP + 4. * zoom + 0.01, "{zoom}: no more than one grid step to spare");
        }
    }

    #[test]
    fn the_buttons_sit_at_the_middle_of_the_zoomed_bar() {
        assert_eq!(top_for(40., 1., 14.), 13.);
        assert_eq!(top_for(40., 0.9, 14.), 11.);
        assert_eq!(top_for(36., 1.5, 14.), 20.);
        assert_eq!(top_for(8., 1., 14.), 0.);
    }
}
