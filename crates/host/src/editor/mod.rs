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

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(windows)]
use windows as imp;

use crate::error::{HostError, Result};

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
