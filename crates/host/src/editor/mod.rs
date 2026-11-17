//! Native top-level windows that hold a plugin's editor.
//!
//! Each editor gets its own window (HWND on Windows, NSWindow on macOS)
//! owned by the main thread. The plugin attaches its view as a child of
//! [`EditorWindow::parent_ptr`] (`IPlugView::attached` with
//! `kPlatformTypeHWND` / `kPlatformTypeNSView`, CLAP `gui.set_parent` with
//! the `win32` / `cocoa` API).
//!
//! The window is passive: it does not call back into the plugin. The host
//! polls [`EditorWindow::client_size`] and [`EditorWindow::user_closed`]
//! from its idle tick, which keeps every plugin call on the main thread and
//! outside of window-procedure re-entrancy.
//!
//! In the app, GPUI's main-thread event loop dispatches messages for these
//! windows. Standalone processes (the sandbox host, tests) call
//! [`pump_messages`] periodically.

pub mod placement;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(windows)]
use windows as imp;

use crate::error::{HostError, Result};
use placement::{Saved, Screen};

/// A copyable reference to an editor window, for host callbacks (VST3
/// `IPlugFrame::resizeView`) that must resize it. Only meaningful on the
/// main thread while the [`EditorWindow`] is alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawWindow(pub(crate) usize);

impl RawWindow {
    /// Resize the client (content) area. Main thread only.
    pub fn set_client_size(self, width: u32, height: u32) {
        #[cfg(any(windows, target_os = "macos"))]
        imp::set_client_size(self.0, width, height);
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = (width, height);
    }
}

pub struct EditorWindow {
    #[cfg(any(windows, target_os = "macos"))]
    inner: imp::Window,
}

impl EditorWindow {
    /// Create a hidden top-level window whose client area is
    /// `width × height` (physical pixels on Windows, points on macOS).
    pub fn new(title: &str, width: u32, height: u32, resizable: bool) -> Result<Self> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            Ok(Self { inner: imp::Window::new(title, width.max(16), height.max(16), resizable)? })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (title, width, height, resizable);
            Err(HostError::Editor("plugin editors are only supported on Windows and macOS".into()))
        }
    }

    /// `HWND` (Windows) or the content `NSView*` (macOS) to attach the
    /// plugin view to.
    pub fn parent_ptr(&self) -> *mut std::ffi::c_void {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.parent_ptr()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            std::ptr::null_mut()
        }
    }

    /// A picture of the editor as drawn now: RGBA (alpha premultiplied), width,
    /// height. Main thread. `None` where it cannot be taken (Windows, for now,
    /// and editors AppKit cannot draw to a bitmap).
    pub fn snapshot(&self) -> Option<(Vec<u8>, u32, u32)> {
        #[cfg(target_os = "macos")]
        {
            self.inner.snapshot()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    pub fn raw(&self) -> RawWindow {
        #[cfg(any(windows, target_os = "macos"))]
        {
            RawWindow(self.inner.raw())
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            RawWindow(0)
        }
    }

    pub fn show(&self) {
        #[cfg(any(windows, target_os = "macos"))]
        self.inner.show();
    }

    /// Bring an already-open window to the front (including from a sandbox).
    pub fn raise(&self) {
        #[cfg(any(windows, target_os = "macos"))]
        self.inner.raise();
    }

    pub fn set_client_size(&self, width: u32, height: u32) {
        self.raw().set_client_size(width, height);
    }

    pub fn client_size(&self) -> (u32, u32) {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.client_size()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            (0, 0)
        }
    }

    /// The top-left corner of the window frame, in screen space (macOS: y
    /// grows upwards; elsewhere downwards, see [`Y_UP`]).
    pub fn top_left(&self) -> (f64, f64) {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.top_left()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            (0.0, 0.0)
        }
    }

    /// Move the window so its frame's top-left corner is at `(x, y)`, and
    /// keep it there when it is shown (otherwise it opens centred).
    pub fn place(&self, x: f64, y: f64) {
        #[cfg(any(windows, target_os = "macos"))]
        self.inner.place(x, y);
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = (x, y);
    }

    /// Allow or forbid dragging the window's edges (a plugin may only know
    /// whether it can be resized once its view is attached).
    pub fn set_resizable(&self, resizable: bool) {
        #[cfg(any(windows, target_os = "macos"))]
        self.inner.set_resizable(resizable);
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = resizable;
    }

    /// The smallest and (when the plugin has one) largest content size the
    /// user can drag the window to.
    pub fn set_limits(&self, min: (u32, u32), max: Option<(u32, u32)>) {
        #[cfg(any(windows, target_os = "macos"))]
        self.inner.set_limits(min, max);
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = (min, max);
    }

    /// What to remember about the window now: its place and content size.
    pub fn placement(&self) -> Saved {
        let (x, y) = self.top_left();
        let (w, h) = self.client_size();
        Saved { x, y, w, h }
    }

    /// The user clicked the close button. The window is hidden but still
    /// exists, so the host can detach the plugin view before destroying it.
    pub fn user_closed(&self) -> bool {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.user_closed()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            false
        }
    }

    /// Display scale (1.0 = 96 DPI on Windows; backing scale on macOS).
    pub fn scale_factor(&self) -> f64 {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.scale_factor()
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            1.0
        }
    }
}

/// Whether the screen space of [`EditorWindow::top_left`] grows upwards
/// (macOS) or downwards (Windows).
pub const Y_UP: bool = cfg!(target_os = "macos");

/// The displays' rectangles in the space of [`EditorWindow::top_left`].
pub fn screens() -> Vec<Screen> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        imp::screens()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Vec::new()
    }
}

/// Where a window of `size` should be placed to restore `saved`: its corner
/// when enough of the title bar is on a display, else `None` (open centred).
pub fn restorable(saved: &Saved) -> Option<(f64, f64)> {
    placement::is_reachable(saved.x, saved.y, f64::from(saved.w), &screens(), Y_UP).then_some((saved.x, saved.y))
}

/// Dispatch pending window messages for this thread (for processes that do
/// not run their own UI event loop).
pub fn pump_messages() {
    #[cfg(any(windows, target_os = "macos"))]
    imp::pump_messages();
}

/// The screen is locked (macOS): editor windows are off screen and OpenGL
/// editors may stall the main thread until it is unlocked. Always `false`
/// elsewhere.
pub fn screen_locked() -> bool {
    #[cfg(target_os = "macos")]
    {
        imp::screen_locked()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

#[allow(dead_code)]
fn unsupported() -> HostError {
    HostError::Editor("unsupported platform".into())
}
