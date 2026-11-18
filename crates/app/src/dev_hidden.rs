//! `AITRACK_DEV_HIDDEN=1`: visual checks in the background. Every window
//! opens off screen, without taking the focus, the mouse or a Dock icon, so
//! `screencapture -l <window id>` can shoot it while the person keeps working
//! in front of their own apps.
//!
//! On macOS the window is raised to level 25: GPUI's backend keeps drawing a
//! window at that level whenever it is ordered in, where an ordinary window
//! moved off screen would be reported occluded and stop getting frames. AppKit
//! keeps a corner of every titled window on screen, so the window class's
//! `constrainFrameRect:toScreen:` is replaced, in this run only, by one that
//! leaves the frame alone.

/// Whether this run is a hidden visual check.
pub fn enabled() -> bool {
    std::env::var_os("AITRACK_DEV_HIDDEN").is_some()
}

/// Move `window` out of sight, if this is a hidden run: all of it but one
/// pixel lies below and left of the main display. That pixel keeps the window
/// on a screen, which AppKit needs to keep giving it frames (fully off screen
/// it stops drawing and cannot be captured).
#[cfg(target_os = "macos")]
pub fn hide(window: &gpui::Window) {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{Encode, Encoding, class, msg_send, sel};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    unsafe impl Encode for Point {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[Point::ENCODING, Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING])]);
    }
    extern "C-unwind" fn unconstrained(_this: *mut AnyObject, _sel: Sel, rect: Rect, _screen: *mut AnyObject) -> Rect {
        rect
    }

    if !enabled() {
        return;
    }
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return };
    unsafe {
        let view = handle.ns_view.as_ptr() as *const AnyObject;
        let ns_window: *const AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let class = objc2::ffi::object_getClass(ns_window) as *mut AnyClass;
        let imp: Imp = std::mem::transmute(unconstrained as extern "C-unwind" fn(*mut AnyObject, Sel, Rect, *mut AnyObject) -> Rect);
        objc2::ffi::class_replaceMethod(class, sel!(constrainFrameRect:toScreen:), imp, c"{CGRect={CGPoint=dd}{CGSize=dd}}@:{CGRect={CGPoint=dd}{CGSize=dd}}@".as_ptr());
        let _: () = msg_send![ns_window, setLevel: 25isize];
        let _: () = msg_send![ns_window, setIgnoresMouseEvents: Bool::YES];
        let frame: Rect = msg_send![ns_window, frame];
        let _: () = msg_send![ns_window, setFrameOrigin: Point { x: 1. - frame.w, y: 1. - frame.h }];
        // Accessory: no Dock icon and never the active app.
        let app: *const AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![app, setActivationPolicy: 1isize];
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide(_window: &gpui::Window) {}
