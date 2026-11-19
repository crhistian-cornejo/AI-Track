//! Cocoa editor windows (NSWindow + content NSView).

use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSEventMask, NSScreen, NSView, NSWindow, NSWindowAnimationBehavior, NSWindowStyleMask};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};

use super::placement::Screen;
use crate::error::{HostError, Result};

pub(super) struct Window {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
    shown: std::cell::Cell<bool>,
    /// The caller put the window somewhere: `show` must not centre it.
    placed: std::cell::Cell<bool>,
}

fn mtm() -> Result<MainThreadMarker> {
    MainThreadMarker::new().ok_or_else(|| HostError::Editor("editor windows must be created on the main thread".into()))
}

impl Window {
    pub fn new(title: &str, width: u32, height: u32, resizable: bool) -> Result<Self> {
        let mtm = mtm()?;
        let mut style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable;
        if resizable {
            style |= NSWindowStyleMask::Resizable;
        }
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width as f64, height as f64));
        let window = unsafe { NSWindow::initWithContentRect_styleMask_backing_defer(mtm.alloc(), rect, style, NSBackingStoreType::Buffered, false) };
        // Centre before attaching so AppKit's screen-frame constraint never
        // shrinks a tall editor created near an arbitrary origin.
        window.center();
        window.setContentSize(NSSize::new(width as f64, height as f64));
        // We own the window; closing it must only hide it so the host can
        // detach the plugin view first.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        // No zoom-in when it opens: during that animation the window is a few
        // percent smaller than it is, and a plugin sizing itself to it (or a
        // screenshot) would see a size that is not the editor's.
        window.setAnimationBehavior(NSWindowAnimationBehavior::None);
        let view = window.contentView().ok_or_else(|| HostError::Editor("NSWindow has no content view".into()))?;
        Ok(Self { window, view, shown: std::cell::Cell::new(false), placed: std::cell::Cell::new(false) })
    }

    pub fn parent_ptr(&self) -> *mut c_void {
        Retained::as_ptr(&self.view) as *mut c_void
    }

    pub fn raw(&self) -> usize {
        Retained::as_ptr(&self.window) as usize
    }

    pub fn show(&self) {
        if !self.placed.get() {
            self.window.center();
        }
        self.window.makeKeyAndOrderFront(None);
        self.shown.set(true);
    }

    pub fn raise(&self) {
        self.window.makeKeyAndOrderFront(None);
        self.window.orderFrontRegardless();
    }

    pub fn client_size(&self) -> (u32, u32) {
        let frame = self.view.frame();
        (frame.size.width.max(0.0).round() as u32, frame.size.height.max(0.0).round() as u32)
    }

    pub fn top_left(&self) -> (f64, f64) {
        let f = self.window.frame();
        (f.origin.x, f.origin.y + f.size.height)
    }

    pub fn place(&self, x: f64, y: f64) {
        self.window.setFrameTopLeftPoint(NSPoint::new(x, y));
        self.placed.set(true);
    }

    pub fn set_resizable(&self, resizable: bool) {
        let mut style = self.window.styleMask();
        style.set(NSWindowStyleMask::Resizable, resizable);
        self.window.setStyleMask(style);
    }

    pub fn set_limits(&self, min: (u32, u32), max: Option<(u32, u32)>) {
        self.window.setContentMinSize(NSSize::new(f64::from(min.0), f64::from(min.1)));
        let max = max.map_or(NSSize::new(f64::from(f32::MAX), f64::from(f32::MAX)), |m| NSSize::new(f64::from(m.0), f64::from(m.1)));
        self.window.setContentMaxSize(max);
    }

    pub fn user_closed(&self) -> bool {
        self.shown.get() && !self.window.isVisible()
    }

    pub fn scale_factor(&self) -> f64 {
        self.window.backingScaleFactor()
    }

    /// The content view as it is drawn now, as RGBA with the alpha premultiplied
    /// (`None` when AppKit gives no bitmap). Views drawn with Core Graphics or
    /// AppKit show up; one drawn straight to an OpenGL or Metal layer may not.
    pub fn snapshot(&self) -> Option<(Vec<u8>, u32, u32)> {
        let bounds = self.view.bounds();
        let rep = self.view.bitmapImageRepForCachingDisplayInRect(bounds)?;
        self.view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
        let (w, h) = (rep.pixelsWide().max(0) as usize, rep.pixelsHigh().max(0) as usize);
        let (spp, bpr) = (rep.samplesPerPixel().max(0) as usize, rep.bytesPerRow().max(0) as usize);
        if w == 0 || h == 0 || !(3..=4).contains(&spp) || rep.isPlanar() || rep.bitsPerSample() != 8 || bpr < w * spp {
            return None;
        }
        let data = rep.bitmapData();
        if data.is_null() {
            return None;
        }
        // SAFETY: AppKit owns `bytesPerRow * pixelsHigh` bytes at `bitmapData` for as long as `rep` lives.
        let src = unsafe { std::slice::from_raw_parts(data, bpr * h) };
        let alpha_first = rep.bitmapFormat().contains(objc2_app_kit::NSBitmapFormat::AlphaFirst);
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for px in src[y * bpr..y * bpr + w * spp].chunks_exact(spp) {
                match (spp, alpha_first) {
                    (4, false) => out.extend_from_slice(px),
                    (4, true) => out.extend_from_slice(&[px[1], px[2], px[3], px[0]]),
                    _ => out.extend_from_slice(&[px[0], px[1], px[2], 255]),
                }
            }
        }
        Some((out, w as u32, h as u32))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.orderOut(None);
        self.window.close();
    }
}

/// Compute the new macOS window frame `(x, y, w, h)` for a client resize,
/// anchoring the top-left corner (unlike `NSWindow::setContentSize`, which
/// anchors bottom-left and can push the title bar above the menu bar, causing
/// AppKit to silently clip the window's height). Keeps the full requested
/// `new_frame_size` intact and shifts the origin into `visible` when needed.
pub(crate) fn frame_for_client_resize(
    old_frame: (f64, f64, f64, f64),
    new_frame_size: (f64, f64),
    visible: Option<(f64, f64, f64, f64)>,
) -> (f64, f64, f64, f64) {
    let (ox, oy, _, oh) = old_frame;
    let (nw, nh) = new_frame_size;
    let mut x = ox;
    let mut y = oy + oh - nh;
    if let Some((vx, vy, vw, vh)) = visible {
        let top = y + nh;
        let max_top = vy + vh;
        if top > max_top {
            y = max_top - nh;
        }
        if y < vy {
            y = (max_top - nh).max(vy);
        }
        if x + nw > vx + vw {
            x = (vx + vw - nw).max(vx);
        }
        if x < vx {
            x = vx;
        }
    }
    (x, y, nw, nh)
}

pub(super) fn set_client_size(raw: usize, width: u32, height: u32) {
    if raw == 0 {
        return;
    }
    // SAFETY: `raw` comes from `Window::raw` and is only used while the
    // owning `Window` (which retains the NSWindow) is alive.
    let window = unsafe { &*(raw as *const NSWindow) };
    let old = window.frame();
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width as f64, height as f64));
    let needed = window.frameRectForContentRect(content);
    let visible = window.screen().map(|s| {
        let v = s.visibleFrame();
        (v.origin.x, v.origin.y, v.size.width, v.size.height)
    });
    let (nx, ny, nw, nh) = frame_for_client_resize(
        (old.origin.x, old.origin.y, old.size.width, old.size.height),
        (needed.size.width, needed.size.height),
        visible,
    );
    window.setFrame_display(NSRect::new(NSPoint::new(nx, ny), NSSize::new(nw, nh)), true);
}

/// The displays, with the top edge as `y` (AppKit's y grows upwards).
pub(super) fn screens() -> Vec<Screen> {
    let Some(mtm) = MainThreadMarker::new() else { return Vec::new() };
    NSScreen::screens(mtm)
        .iter()
        .map(|s| {
            let f = s.frame();
            Screen { x: f.origin.x, y: f.origin.y + f.size.height, w: f.size.width, h: f.size.height }
        })
        .collect()
}

/// Most events handled per call, and the longest a call may take: an editor
/// that keeps posting redraws (DPF's OpenGL views do) would otherwise keep
/// the queue from ever running dry and the call from returning.
const PUMP_MAX_EVENTS: usize = 64;
const PUMP_MAX_TIME: std::time::Duration = std::time::Duration::from_millis(8);

pub(super) fn pump_messages() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let start = std::time::Instant::now();
    for _ in 0..PUMP_MAX_EVENTS {
        if start.elapsed() >= PUMP_MAX_TIME {
            break;
        }
        let event =
            unsafe { app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&NSDate::distantPast()), NSDefaultRunLoopMode, true) };
        match event {
            Some(e) => app.sendEvent(&e),
            None => break,
        }
    }
}

/// The login session's screen is locked. While it is, windows are off
/// screen and OpenGL editors (JUCE, DPF) can spend the main thread redrawing
/// surfaces that never present, so editors cannot be checked.
pub(super) fn screen_locked() -> bool {
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::CFString;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
    }
    // SAFETY: a Copy-rule function: we own the returned dictionary (or null
    // without a window server session).
    let raw = unsafe { CGSessionCopyCurrentDictionary() };
    if raw.is_null() {
        return false;
    }
    let dict: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_create_rule(raw) };
    dict.find(CFString::from_static_string("CGSSessionScreenIsLocked"))
        .and_then(|v| v.downcast::<CFBoolean>())
        .is_some_and(bool::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_resize_anchors_top_left_and_preserves_full_size() {
        let visible = Some((0., 40., 1440., 835.));
        // Growing taller keeps the top edge fixed (old top = 300 + 400 = 700).
        let (x, y, w, h) = frame_for_client_resize((200., 300., 600., 400.), (600., 500.), visible);
        assert_eq!((x, y, w, h), (200., 200., 600., 500.));
        assert_eq!(y + h, 700., "top edge stays put");
        // Growing past the bottom of the visible screen shifts up without clipping height.
        let (_, y2, w2, h2) = frame_for_client_resize((200., 100., 600., 400.), (600., 520.), visible);
        assert_eq!((w2, h2), (600., 520.), "requested height is never clipped");
        assert!(y2 >= 40. && y2 + h2 <= 875.);
    }
}
