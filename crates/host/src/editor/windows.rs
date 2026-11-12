//! Win32 editor windows.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, GetSysColorBrush};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::error::{HostError, Result};

const CLASS_NAME: PCWSTR = w!("AITrackPluginEditor");

fn register_class() -> Result<()> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return false };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: module.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        RegisterClassExW(&class) != 0
    });
    if ok { Ok(()) } else { Err(HostError::Editor("RegisterClassExW failed".into())) }
}

/// Per-window state reachable from the window procedure through
/// `GWLP_USERDATA`. Freed in `Window::drop` after `DestroyWindow`.
struct WindowState {
    closed: AtomicBool,
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_CLOSE {
            // Don't destroy: the host must detach the plugin view first.
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
            if let Some(state) = state.as_ref() {
                state.closed.store(true, Ordering::Release);
            }
            let _ = ShowWindow(hwnd, SW_HIDE);
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

pub(super) struct Window {
    hwnd: HWND,
    state: *mut WindowState,
    style: WINDOW_STYLE,
}

impl Window {
    pub fn new(title: &str, width: u32, height: u32, resizable: bool) -> Result<Self> {
        register_class()?;
        let mut style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_CLIPCHILDREN;
        if resizable {
            style |= WS_THICKFRAME | WS_MAXIMIZEBOX;
        }
        let (w, h) = outer_size(style, width, height, 96);
        let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let state = Box::into_raw(Box::new(WindowState { closed: AtomicBool::new(false) }));
        let hwnd = unsafe {
            let module = GetModuleHandleW(None).map_err(|e| HostError::Editor(e.to_string()))?;
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                CLASS_NAME,
                PCWSTR(title.as_ptr()),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                w,
                h,
                None,
                None,
                Some(module.into()),
                None,
            )
        };
        let hwnd = match hwnd {
            Ok(h) => h,
            Err(e) => {
                drop(unsafe { Box::from_raw(state) });
                return Err(HostError::Editor(format!("CreateWindowExW failed: {e}")));
            }
        };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };
        let win = Self { hwnd, state, style };
        // Now that the window exists we know its monitor DPI; fix the outer
        // size so the client area is exactly what the plugin asked for.
        set_client_size(win.raw(), width, height);
        Ok(win)
    }

    pub fn parent_ptr(&self) -> *mut c_void {
        self.hwnd.0
    }

    pub fn raw(&self) -> usize {
        self.hwnd.0 as usize
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn client_size(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rect);
        }
        ((rect.right - rect.left).max(0) as u32, (rect.bottom - rect.top).max(0) as u32)
    }

    pub fn user_closed(&self) -> bool {
        unsafe { (*self.state).closed.load(Ordering::Acquire) }
    }

    pub fn scale_factor(&self) -> f64 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
    }

    #[allow(dead_code)]
    pub fn style(&self) -> WINDOW_STYLE {
        self.style
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            drop(Box::from_raw(self.state));
        }
    }
}

fn outer_size(style: WINDOW_STYLE, width: u32, height: u32, dpi: u32) -> (i32, i32) {
    let mut rect = RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 };
    unsafe {
        let _ = windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi(&mut rect, style, false, WINDOW_EX_STYLE::default(), dpi);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

pub(super) fn set_client_size(raw: usize, width: u32, height: u32) {
    let hwnd = HWND(raw as *mut c_void);
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
        let dpi = GetDpiForWindow(hwnd);
        let (w, h) = outer_size(style, width, height, if dpi == 0 { 96 } else { dpi });
        let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Most messages handled per call: an editor that keeps posting messages
/// must not keep the call from returning (see the macOS side).
const PUMP_MAX_EVENTS: usize = 64;

pub(super) fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        for _ in 0..PUMP_MAX_EVENTS {
            if !PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
