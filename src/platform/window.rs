//! Direct control over the translator popup's native window: the framework has no
//! hide/show or placement API for it.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, GetForegroundWindow, GetWindowRect, SW_HIDE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
};

/// `HWND` is not `Send`, and the popup handle never leaves the UI thread; this keeps
/// it plain data so it can be captured by UI callbacks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Handle(isize);

impl Handle {
    pub fn of(window: &gpui_kit::Window) -> Option<Self> {
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(Self(handle.hwnd.get())),
            _ => None,
        }
    }

    fn hwnd(self) -> HWND {
        HWND(self.0 as _)
    }

    pub fn show(self) {
        let _ = unsafe { ShowWindow(self.hwnd(), SW_SHOW) };
    }

    pub fn hide(self) {
        let _ = unsafe { ShowWindow(self.hwnd(), SW_HIDE) };
    }

    pub fn is_foreground(self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd() }
    }

    /// Windows 11 rounded corners; older systems ignore the request.
    pub fn round_corners(self) {
        let preference = DWMWCP_ROUND;
        let _ = unsafe {
            DwmSetWindowAttribute(
                self.hwnd(),
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&raw const preference).cast(),
                size_of_val(&preference) as u32,
            )
        };
    }

    /// Resizes the client area (physical pixels) and keeps the whole window inside
    /// the work area. With `near_cursor`, also moves it just below the mouse
    /// pointer, or above it when there is no room below.
    pub fn fit(self, width: i32, height: i32, near_cursor: bool) {
        const GAP: i32 = 16;
        let hwnd = self.hwnd();
        let (mut outer, mut client) = (RECT::default(), RECT::default());
        unsafe {
            if GetWindowRect(hwnd, &mut outer).is_err() || GetClientRect(hwnd, &mut client).is_err()
            {
                return;
            }
        }
        let width = width + (outer.right - outer.left) - (client.right - client.left);
        let height = height + (outer.bottom - outer.top) - (client.bottom - client.top);

        let mut cursor = POINT::default();
        let monitor = if near_cursor && unsafe { GetCursorPos(&mut cursor) }.is_ok() {
            unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) }
        } else {
            unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }
        };
        let Some(work) = work_area(monitor) else {
            return;
        };
        let (x, y) = if near_cursor {
            let below = cursor.y + GAP;
            let y = if below + height <= work.bottom {
                below
            } else {
                cursor.y - GAP - height
            };
            (cursor.x - width / 2, y)
        } else {
            (outer.left, outer.top)
        };
        let x = x.min(work.right - width).max(work.left);
        let y = y.min(work.bottom - height).max(work.top);
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
}

fn work_area(monitor: HMONITOR) -> Option<RECT> {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then_some(info.rcWork)
}
