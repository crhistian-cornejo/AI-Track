//! Cocoa editor windows (NSWindow + content NSView).

use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSEventMask, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize, NSString};

use crate::error::{HostError, Result};

pub(super) struct Window {
    window: Retained<NSWindow>,
    view: Retained<NSView>,
    shown: std::cell::Cell<bool>,
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
        let rect = NSRect::new(NSPoint::new(200.0, 200.0), NSSize::new(width as f64, height as f64));
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(mtm.alloc(), rect, style, NSBackingStoreType::Buffered, false)
        };
        // We own the window; closing it must only hide it so the host can
        // detach the plugin view first.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        let view = window.contentView().ok_or_else(|| HostError::Editor("NSWindow has no content view".into()))?;
        Ok(Self { window, view, shown: std::cell::Cell::new(false) })
    }

    pub fn parent_ptr(&self) -> *mut c_void {
        Retained::as_ptr(&self.view) as *mut c_void
    }

    pub fn raw(&self) -> usize {
        Retained::as_ptr(&self.window) as usize
    }

    pub fn show(&self) {
        self.window.center();
        self.window.makeKeyAndOrderFront(None);
        self.shown.set(true);
    }

    pub fn client_size(&self) -> (u32, u32) {
        let frame = self.view.frame();
        (frame.size.width.max(0.0) as u32, frame.size.height.max(0.0) as u32)
    }

    pub fn user_closed(&self) -> bool {
        self.shown.get() && !self.window.isVisible()
    }

    pub fn scale_factor(&self) -> f64 {
        self.window.backingScaleFactor()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.window.orderOut(None);
        self.window.close();
    }
}

pub(super) fn set_client_size(raw: usize, width: u32, height: u32) {
    if raw == 0 {
        return;
    }
    // SAFETY: `raw` comes from `Window::raw` and is only used while the
    // owning `Window` (which retains the NSWindow) is alive.
    let window = unsafe { &*(raw as *const NSWindow) };
    window.setContentSize(NSSize::new(width as f64, height as f64));
}

pub(super) fn pump_messages() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    loop {
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(NSEventMask::Any, Some(&NSDate::distantPast()), NSDefaultRunLoopMode, true)
        };
        match event {
            Some(e) => app.sendEvent(&e),
            None => break,
        }
    }
}
