use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_INSERT,
    VK_LWIN, VK_MBUTTON, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
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

/// Classic console windows are drawn by conhost itself and host no child console.
const CONSOLE_CLASSES: &[&str] = &["ConsoleWindowClass", "PuTTY", "mintty"];

/// Whether the foreground app runs a terminal, where Ctrl+C without a selection
/// interrupts the running program. Modern terminals, Windows Terminal and editors
/// with an integrated terminal alike, host shells through a pseudoconsole, which
/// shows up as a `conhost.exe` / `OpenConsole.exe` child of their process.
pub fn foreground_is_terminal() -> bool {
    let window = unsafe { GetForegroundWindow() };
    let mut buffer = [0u16; 64];
    let length = unsafe { GetClassNameW(window, &mut buffer) };
    let class = String::from_utf16_lossy(&buffer[..length.max(0) as usize]);
    if CONSOLE_CLASSES.contains(&class.as_str()) {
        return true;
    }
    let mut process = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut process)) };
    process != 0 && hosts_pseudoconsole(process)
}

fn hosts_pseudoconsole(process: u32) -> bool {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return false;
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = false;
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more && !found {
        if entry.th32ParentProcessID == process {
            let length = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            found = name.eq_ignore_ascii_case("conhost.exe")
                || name.eq_ignore_ascii_case("OpenConsole.exe");
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    found
}

/// Which copy shortcut to send. Ctrl+Insert is the CUA copy that browsers, Office,
/// Qt, WPF, Java and terminals all accept, and it never interrupts a program.
#[derive(Clone, Copy)]
pub enum CopyKey {
    Insert,
    C,
}

/// Sends the copy shortcut, first releasing modifiers still physically held so the
/// target does not receive Ctrl+Alt+C or similar. One atomic batch.
pub fn send_copy(copy: CopyKey) {
    let letter = match copy {
        CopyKey::Insert => VK_INSERT,
        CopyKey::C => VK_C,
    };
    let mut inputs: Vec<INPUT> = [VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN, VK_CONTROL]
        .into_iter()
        .filter(|&vk| is_down(vk))
        .map(|vk| key(vk, true))
        .collect();
    inputs.extend([
        key(VK_CONTROL, false),
        key(letter, false),
        key(letter, true),
        key(VK_CONTROL, true),
    ]);
    send(&inputs);
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
