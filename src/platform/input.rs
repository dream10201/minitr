use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_INSERT, VK_LWIN,
    VK_MBUTTON, VK_MENU, VK_RWIN, VK_SHIFT,
};

use crate::hotkey::{Key, Modifiers, Trigger};

fn is_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0.into()) as u16 & 0x8000 != 0 }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    // Without the extended flag Insert arrives as numpad 0.
    if vk == VK_INSERT {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
}

/// The foreground app only sees Alt/Win go down and up (the hotkey itself is
/// swallowed) and would move focus to its menu bar; a Ctrl tap masks that.
pub fn mask_modifier_release() {
    if [VK_MENU, VK_LWIN, VK_RWIN].into_iter().any(is_down) {
        send(&[key(VK_CONTROL, false), key(VK_CONTROL, true)]);
    }
}

/// Sends Ctrl+`vk`. Modifiers still physically held are released first so the
/// target does not see Ctrl+Alt+Insert or similar. Ctrl stays down for a moment
/// on both sides of the key: input methods and apps that read the live key state
/// while handling it would otherwise see a bare key and type it.
pub fn send_ctrl(vk: VIRTUAL_KEY) {
    const HOLD: Duration = Duration::from_millis(30);
    let mut inputs: Vec<INPUT> = [VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_CONTROL]
        .into_iter()
        .filter(|&vk| is_down(vk))
        .map(|vk| key(vk, true))
        .collect();
    inputs.push(key(VK_CONTROL, false));
    send(&inputs);
    // On its own thread so that a cancelled capture cannot leave Ctrl held down.
    std::thread::spawn(move || {
        std::thread::sleep(HOLD);
        send(&[key(vk, false), key(vk, true)]);
        std::thread::sleep(HOLD);
        send(&[key(VK_CONTROL, true)]);
    });
}

/// Whether the keys or button that fired `trigger` are still held.
pub fn trigger_held(trigger: Trigger) -> bool {
    match trigger {
        Trigger::MiddleDoubleClick => is_down(VK_MBUTTON),
        Trigger::Chord(_, key) => {
            [VK_MENU, VK_CONTROL, VK_SHIFT].into_iter().any(is_down)
                || is_down(VIRTUAL_KEY(key.vk()))
        }
        Trigger::DoubleTap(_) => [VK_MENU, VK_CONTROL, VK_SHIFT].into_iter().any(is_down),
    }
}

/// The chord being pressed, read from the UI thread's synchronized key state while
/// handling a key-down message. Independent of keyboard layout and Shift mapping.
pub fn pressed_chord() -> Option<(Modifiers, Key)> {
    let held = |vk: u16| unsafe { GetKeyState(vk.into()) as u16 & 0x8000 != 0 };
    let key = Key::all().find(|key| held(key.vk()))?;
    let modifiers = Modifiers {
        ctrl: held(VK_CONTROL.0),
        alt: held(VK_MENU.0),
        shift: held(VK_SHIFT.0),
        win: held(VK_LWIN.0) || held(VK_RWIN.0),
    };
    Some((modifiers, key))
}
