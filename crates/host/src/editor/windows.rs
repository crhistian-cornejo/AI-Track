//! Win32 editor windows.

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, EnumDisplayMonitors, GetMonitorInfoW, GetSysColorBrush, HDC, HMONITOR, MONITORINFO};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::placement::Screen;
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
    /// Content-size limits the user can drag to (0 = none), for `WM_GETMINMAXINFO`.
    min: (AtomicU32, AtomicU32),
    max: (AtomicU32, AtomicU32),
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
        if msg == WM_GETMINMAXINFO {
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
            if let (Some(state), Some(info)) = (state.as_ref(), (lparam.0 as *mut MINMAXINFO).as_mut()) {
                let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
                let dpi = GetDpiForWindow(hwnd);
                let dpi = if dpi == 0 { 96 } else { dpi };
                let (min_w, min_h) = (state.min.0.load(Ordering::Relaxed), state.min.1.load(Ordering::Relaxed));
                if min_w > 0 && min_h > 0 {
                    let (w, h) = outer_size(style, min_w, min_h, dpi);
                    info.ptMinTrackSize = windows::Win32::Foundation::POINT { x: w, y: h };
                }
                let (max_w, max_h) = (state.max.0.load(Ordering::Relaxed), state.max.1.load(Ordering::Relaxed));
                if max_w > 0 && max_h > 0 {
                    let (w, h) = outer_size(style, max_w, max_h, dpi);
                    info.ptMaxTrackSize = windows::Win32::Foundation::POINT { x: w, y: h };
                }
            }
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
        let state = Box::into_raw(Box::new(WindowState { closed: AtomicBool::new(false), min: Default::default(), max: Default::default() }));
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

    pub fn raise(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
            let _ = BringWindowToTop(self.hwnd);
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

    pub fn top_left(&self) -> (f64, f64) {
        let mut rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut rect);
        }
        (f64::from(rect.left), f64::from(rect.top))
    }

    pub fn place(&self, x: f64, y: f64) {
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, x as i32, y as i32, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    pub fn set_resizable(&self, resizable: bool) {
        unsafe {
            let mut style = WINDOW_STYLE(GetWindowLongPtrW(self.hwnd, GWL_STYLE) as u32);
            let (w, h) = self.client_size();
            if resizable {
                style |= WS_THICKFRAME | WS_MAXIMIZEBOX;
            } else {
                style &= !(WS_THICKFRAME | WS_MAXIMIZEBOX);
            }
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, style.0 as isize);
            let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            set_client_size(self.raw(), w, h);
        }
    }

    pub fn set_limits(&self, min: (u32, u32), max: Option<(u32, u32)>) {
        let state = unsafe { &*self.state };
        state.min.0.store(min.0, Ordering::Relaxed);
        state.min.1.store(min.1, Ordering::Relaxed);
        let (mw, mh) = max.unwrap_or((0, 0));
        state.max.0.store(mw, Ordering::Relaxed);
        state.max.1.store(mh, Ordering::Relaxed);
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

unsafe extern "system" fn collect_monitor(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows::core::BOOL {
    let out = unsafe { &mut *(data.0 as *mut Vec<Screen>) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        let r = info.rcMonitor;
        out.push(Screen { x: f64::from(r.left), y: f64::from(r.top), w: f64::from(r.right - r.left), h: f64::from(r.bottom - r.top) });
    }
    true.into()
}

/// The displays, with `y` growing downwards.
pub(super) fn screens() -> Vec<Screen> {
    let mut out: Vec<Screen> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut out as *mut Vec<Screen> as isize));
    }
    out
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
