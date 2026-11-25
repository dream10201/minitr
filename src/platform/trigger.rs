//! Global activation: a registered hotkey, or low-level hooks for gestures
//! `RegisterHotKey` cannot express. Everything runs on the UI thread.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context as _, Result};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetSystemMetrics, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED,
    MSLLHOOKSTRUCT, SM_CXDOUBLECLK, SM_CYDOUBLECLK, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_MBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::hotkey::{DoubleClick, DoubleTap, Modifiers, Trigger};

const HOTKEY_ID: i32 = 0x3572;

enum Detector {
    HotKey,
    Keyboard(HHOOK, DoubleTap),
    Mouse(HHOOK, DoubleClick),
}

struct Active {
    detector: Detector,
    fire: Rc<dyn Fn()>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// Keeps the trigger armed until dropped. Only one exists at a time.
pub struct Listener(());

impl Drop for Listener {
    fn drop(&mut self) {
        let Some(active) = ACTIVE.take() else { return };
        unsafe {
            match active.detector {
                Detector::HotKey => {
                    drop(UnregisterHotKey(Some(super::message_window()), HOTKEY_ID))
                }
                Detector::Keyboard(hook, _) | Detector::Mouse(hook, _) => {
                    drop(UnhookWindowsHookEx(hook))
                }
            }
        }
    }
}

pub fn listen(trigger: Trigger, interval_ms: u32, fire: impl Fn() + 'static) -> Result<Listener> {
    debug_assert!(
        ACTIVE.with_borrow(Option::is_none),
        "previous listener still alive"
    );
    let detector = unsafe {
        match trigger {
            Trigger::Chord(modifiers, key) => {
                RegisterHotKey(
                    Some(super::message_window()),
                    HOTKEY_ID,
                    hotkey_modifiers(modifiers) | MOD_NOREPEAT,
                    key.vk().into(),
                )
                .with_context(|| {
                    format!("热键 {} 注册失败，可能被其他程序占用", trigger.label())
                })?;
                Detector::HotKey
            }
            Trigger::DoubleTap(modifier) => {
                let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module(), 0)
                    .with_context(|| format!("{} 监听安装失败", trigger.label()))?;
                Detector::Keyboard(hook, DoubleTap::new(modifier, interval_ms))
            }
            Trigger::MiddleDoubleClick => {
                let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module(), 0)
                    .context("鼠标中键双击监听安装失败")?;
                let slop = (
                    GetSystemMetrics(SM_CXDOUBLECLK) / 2,
                    GetSystemMetrics(SM_CYDOUBLECLK) / 2,
                );
                Detector::Mouse(hook, DoubleClick::new(interval_ms, slop))
            }
        }
    };
    ACTIVE.set(Some(Active {
        detector,
        fire: Rc::new(fire),
    }));
    Ok(Listener(()))
}

fn module() -> Option<HINSTANCE> {
    unsafe { GetModuleHandleW(None) }.ok().map(Into::into)
}

fn hotkey_modifiers(modifiers: Modifiers) -> HOT_KEY_MODIFIERS {
    [
        (modifiers.ctrl, MOD_CONTROL),
        (modifiers.alt, MOD_ALT),
        (modifiers.shift, MOD_SHIFT),
        (modifiers.win, MOD_WIN),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(HOT_KEY_MODIFIERS(0), |acc, (_, flag)| acc | flag)
}

/// Runs the detector on the active listener and, outside the borrow, the callback.
fn feed(detect: impl FnOnce(&mut Detector) -> bool) {
    let fire = ACTIVE.with_borrow_mut(|active| {
        let active = active.as_mut()?;
        detect(&mut active.detector).then(|| active.fire.clone())
    });
    if let Some(fire) = fire {
        fire();
    }
}

/// Whether the double-tapped modifier is physically down, as last seen by the hook.
pub fn tap_held() -> bool {
    ACTIVE.with_borrow(|active| {
        matches!(active, Some(Active { detector: Detector::Keyboard(_, tap), .. }) if tap.held())
    })
}

pub(super) fn on_hotkey(id: i32) {
    if id == HOTKEY_ID {
        feed(|detector| matches!(detector, Detector::HotKey));
    }
}

// Synthesized input is ignored: our own Ctrl+C and the framework's focus-stealing
// Alt tap must not count as presses.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let message = wparam.0 as u32;
        let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let up = matches!(message, WM_KEYUP | WM_SYSKEYUP);
        if (down || up) && !info.flags.contains(LLKHF_INJECTED) {
            feed(|detector| match detector {
                Detector::Keyboard(_, tap) => tap.key(info.vkCode, down, info.time),
                _ => false,
            });
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && wparam.0 as u32 == WM_MBUTTONDOWN {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 {
            feed(|detector| match detector {
                Detector::Mouse(_, click) => click.press(info.time, info.pt.x, info.pt.y),
                _ => false,
            });
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
