//! Win32 services the UI framework does not cover.

pub mod autostart;
pub mod clipboard;
pub mod input;
pub mod instance;
pub mod selection;
pub mod speech;
pub mod trigger;
pub mod window;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HWND_MESSAGE, RegisterClassW, WINDOW_EX_STYLE, WM_HOTKEY,
    WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::w;

thread_local! {
    static MESSAGE_WINDOW: HWND = create_message_window();
}

/// A hidden message-only window on the UI thread: receives `WM_HOTKEY` and owns
/// clipboard writes.
fn message_window() -> HWND {
    MESSAGE_WINDOW.with(|hwnd| *hwnd)
}

fn create_message_window() -> HWND {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW {
            lpfnWndProc: Some(message_proc),
            hInstance: instance.into(),
            lpszClassName: w!("MiniTranslation.Message"),
            ..Default::default()
        };
        RegisterClassW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class.lpszClassName,
            None,
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
        .expect("failed to create the message window")
    }
}

unsafe extern "system" fn message_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_HOTKEY {
        trigger::on_hotkey(wparam.0 as i32);
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
