use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Modifiers {
    pub fn is_empty(self) -> bool {
        !(self.ctrl || self.alt || self.shift || self.win)
    }

    fn names(self) -> impl Iterator<Item = &'static str> {
        [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.win, "Win"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
}

impl Modifier {
    fn name(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
        }
    }
}

/// A non-modifier key, identified by its Windows virtual-key code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key(u16);

/// `(virtual-key, config name, display label)`; config names follow .NET `Keys`
/// so settings written by earlier releases keep parsing.
const SPECIAL_KEYS: &[(u16, &str, &str)] = &[
    (0x08, "Back", "Backspace"),
    (0x09, "Tab", "Tab"),
    (0x0D, "Enter", "Enter"),
    (0x13, "Pause", "Pause"),
    (0x20, "Space", "Space"),
    (0x21, "PageUp", "PgUp"),
    (0x22, "PageDown", "PgDn"),
    (0x23, "End", "End"),
    (0x24, "Home", "Home"),
    (0x25, "Left", "←"),
    (0x26, "Up", "↑"),
    (0x27, "Right", "→"),
    (0x28, "Down", "↓"),
    (0x2C, "PrintScreen", "PrtSc"),
    (0x2D, "Insert", "Insert"),
    (0x2E, "Delete", "Delete"),
    (0x6A, "Multiply", "Num *"),
    (0x6B, "Add", "Num +"),
    (0x6D, "Subtract", "Num -"),
    (0x6E, "Decimal", "Num ."),
    (0x6F, "Divide", "Num /"),
    (0x91, "Scroll", "ScrLk"),
    (0xBA, "Oem1", ";"),
    (0xBB, "Oemplus", "="),
    (0xBC, "Oemcomma", ","),
    (0xBD, "OemMinus", "-"),
    (0xBE, "OemPeriod", "."),
    (0xBF, "OemQuestion", "/"),
    (0xC0, "Oemtilde", "`"),
    (0xDB, "OemOpenBrackets", "["),
    (0xDC, "Oem5", "\\"),
    (0xDD, "Oem6", "]"),
    (0xDE, "Oem7", "'"),
];

const KEY_ALIASES: &[(&str, u16)] = &[
    ("backspace", 0x08),
    ("return", 0x0D),
    ("prior", 0x21),
    ("next", 0x22),
    ("snapshot", 0x2C),
    ("oemsemicolon", 0xBA),
    ("oem2", 0xBF),
    ("oem3", 0xC0),
    ("oem4", 0xDB),
    ("oempipe", 0xDC),
    ("oemclosebrackets", 0xDD),
    ("oemquotes", 0xDE),
];

impl Key {
    pub const fn vk(self) -> u16 {
        self.0
    }

    pub fn from_vk(vk: u16) -> Option<Self> {
        let supported = matches!(vk, 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x69 | 0x70..=0x87)
            || SPECIAL_KEYS.iter().any(|&(code, ..)| code == vk);
        supported.then_some(Self(vk))
    }

    /// Every key a trigger can be bound to.
    pub fn all() -> impl Iterator<Item = Self> {
        (0x30..=0x39)
            .chain(0x41..=0x5A)
            .chain(0x60..=0x69)
            .chain(0x70..=0x87)
            .chain(SPECIAL_KEYS.iter().map(|&(vk, ..)| vk))
            .map(Self)
    }

    pub fn is_function_key(self) -> bool {
        (0x70..=0x87).contains(&self.0)
    }

    fn name(self) -> String {
        match self.0 {
            vk @ 0x30..=0x39 => format!("D{}", vk - 0x30),
            vk @ 0x41..=0x5A => char::from(vk as u8).to_string(),
            vk @ 0x60..=0x69 => format!("NumPad{}", vk - 0x60),
            vk @ 0x70..=0x87 => format!("F{}", vk - 0x6F),
            vk => special(vk).map_or_else(|| format!("0x{vk:02X}"), |(name, _)| name.into()),
        }
    }

    fn label(self) -> String {
        match self.0 {
            vk @ 0x30..=0x39 => (vk - 0x30).to_string(),
            vk @ 0x60..=0x69 => format!("Num {}", vk - 0x60),
            vk => special(vk).map_or_else(|| self.name(), |(_, label)| label.into()),
        }
    }

    fn parse(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        let number = |prefix: &str| lower.strip_prefix(prefix)?.parse::<u16>().ok();
        let vk = match lower.as_bytes() {
            [c @ b'a'..=b'z'] => u16::from(c.to_ascii_uppercase()),
            [c @ b'0'..=b'9'] => u16::from(*c),
            _ => {
                if let Some(n) = number("numpad").filter(|n| *n <= 9) {
                    0x60 + n
                } else if let Some(n) = number("f").filter(|n| (1..=24).contains(n)) {
                    0x6F + n
                } else if let Some(n) = number("d").filter(|n| *n <= 9) {
                    0x30 + n
                } else {
                    SPECIAL_KEYS
                        .iter()
                        .find(|(_, special, _)| special.eq_ignore_ascii_case(&lower))
                        .map(|&(vk, ..)| vk)
                        .or_else(|| {
                            KEY_ALIASES
                                .iter()
                                .find(|(a, _)| *a == lower)
                                .map(|&(_, vk)| vk)
                        })?
                }
            }
        };
        Self::from_vk(vk)
    }
}

fn special(vk: u16) -> Option<(&'static str, &'static str)> {
    SPECIAL_KEYS
        .iter()
        .find(|&&(code, ..)| code == vk)
        .map(|&(_, name, label)| (name, label))
}

/// What summons the translator window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Chord(Modifiers, Key),
    DoubleTap(Modifier),
    MiddleDoubleClick,
}

impl Default for Trigger {
    fn default() -> Self {
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        Self::Chord(alt, Key(0x51))
    }
}

impl Trigger {
    /// A chord is only usable globally when it cannot swallow ordinary typing.
    pub fn chord(modifiers: Modifiers, key: Key) -> Option<Self> {
        (!modifiers.is_empty() || key.is_function_key()).then_some(Self::Chord(modifiers, key))
    }

    pub fn label(&self) -> String {
        match self {
            Self::Chord(modifiers, key) => {
                let mut parts: Vec<String> = modifiers.names().map(String::from).collect();
                parts.push(key.label());
                parts.join(" + ")
            }
            Self::DoubleTap(modifier) => format!("双击 {}", modifier.name()),
            Self::MiddleDoubleClick => "双击鼠标中键".into(),
        }
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chord(modifiers, key) => {
                for name in modifiers.names() {
                    write!(f, "{name}+")?;
                }
                f.write_str(&key.name())
            }
            Self::DoubleTap(modifier) => write!(f, "{}Double", modifier.name()),
            Self::MiddleDoubleClick => f.write_str("MouseMiddleDouble"),
        }
    }
}

impl FromStr for Trigger {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        let text = text.trim();
        let double = |name: &str| text.eq_ignore_ascii_case(&format!("{name}Double"));
        if text.eq_ignore_ascii_case("MouseMiddleDouble") {
            return Ok(Self::MiddleDoubleClick);
        }
        for modifier in [Modifier::Ctrl, Modifier::Alt, Modifier::Shift] {
            if double(modifier.name()) {
                return Ok(Self::DoubleTap(modifier));
            }
        }

        let mut modifiers = Modifiers::default();
        let mut key = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers.ctrl = true,
                "alt" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                "win" => modifiers.win = true,
                _ if key.is_none() => key = Some(Key::parse(part).ok_or(())?),
                _ => return Err(()),
            }
        }
        key.map(|key| Self::Chord(modifiers, key)).ok_or(())
    }
}

impl Serialize for Trigger {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Trigger {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(text.parse().unwrap_or_default())
    }
}

impl Modifier {
    /// Matches the generic and the left/right virtual-key codes.
    fn matches_vk(self, vk: u32) -> bool {
        match self {
            Self::Ctrl => matches!(vk, 0x11 | 0xA2 | 0xA3),
            Self::Alt => matches!(vk, 0x12 | 0xA4 | 0xA5),
            Self::Shift => matches!(vk, 0x10 | 0xA0 | 0xA1),
        }
    }
}

/// Recognizes a lone modifier tapped twice. A tap used as part of a chord does not
/// count, and a third tap does not fire again.
pub struct DoubleTap {
    modifier: Modifier,
    interval_ms: u32,
    held: bool,
    chorded: bool,
    fired: bool,
    armed_at: Option<u32>,
}

impl DoubleTap {
    pub fn new(modifier: Modifier, interval_ms: u32) -> Self {
        Self {
            modifier,
            interval_ms,
            held: false,
            chorded: false,
            fired: false,
            armed_at: None,
        }
    }

    pub fn held(&self) -> bool {
        self.held
    }

    /// Feeds one key event (`time` in milliseconds); returns whether the trigger fires.
    pub fn key(&mut self, vk: u32, down: bool, time: u32) -> bool {
        if !self.modifier.matches_vk(vk) {
            if down {
                self.chorded = true;
                self.armed_at = None;
            }
            return false;
        }
        if !down {
            self.held = false;
            self.armed_at = (!self.fired && !self.chorded).then_some(time);
            self.fired = false;
            return false;
        }
        if std::mem::replace(&mut self.held, true) {
            return false;
        }
        self.chorded = false;
        self.fired = self
            .armed_at
            .take()
            .is_some_and(|at| time.wrapping_sub(at) <= self.interval_ms);
        self.fired
    }
}

/// Recognizes two presses close in time and space. A third press starts over.
pub struct DoubleClick {
    interval_ms: u32,
    slop: (i32, i32),
    last: Option<(u32, i32, i32)>,
}

impl DoubleClick {
    pub fn new(interval_ms: u32, slop: (i32, i32)) -> Self {
        Self {
            interval_ms,
            slop,
            last: None,
        }
    }

    pub fn press(&mut self, time: u32, x: i32, y: i32) -> bool {
        let fires = self.last.take().is_some_and(|(at, lx, ly)| {
            time.wrapping_sub(at) <= self.interval_ms
                && (x - lx).abs() <= self.slop.0
                && (y - ly).abs() <= self.slop.1
        });
        if !fires {
            self.last = Some((time, x, y));
        }
        fires
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_form() {
        for text in [
            "Alt+Q",
            "Ctrl+Shift+F1",
            "Ctrl+Alt+Shift+Win+D5",
            "F12",
            "Alt+Oemtilde",
            "Ctrl+NumPad3",
            "CtrlDouble",
            "ShiftDouble",
            "MouseMiddleDouble",
        ] {
            let trigger: Trigger = text.parse().unwrap();
            assert_eq!(trigger.to_string(), text);
        }
    }

    #[test]
    fn accepts_legacy_aliases_and_case() {
        assert_eq!("alt+q".parse::<Trigger>().unwrap().to_string(), "Alt+Q");
        assert_eq!(
            "Control+Return".parse::<Trigger>().unwrap().to_string(),
            "Ctrl+Enter"
        );
        assert_eq!(
            "Alt+OemCloseBrackets"
                .parse::<Trigger>()
                .unwrap()
                .to_string(),
            "Alt+Oem6"
        );
        assert_eq!("Alt+3".parse::<Trigger>().unwrap().to_string(), "Alt+D3");
        assert_eq!(
            "mousemiddledouble".parse::<Trigger>(),
            Ok(Trigger::MiddleDoubleClick)
        );
    }

    #[test]
    fn rejects_garbage() {
        for text in ["", "Alt", "Alt+Q+W", "Alt+NoSuchKey", "F25"] {
            assert!(text.parse::<Trigger>().is_err(), "{text}");
        }
    }

    #[test]
    fn chord_requires_modifier_unless_function_key() {
        let none = Modifiers::default();
        assert!(Trigger::chord(none, Key::parse("Q").unwrap()).is_none());
        assert!(Trigger::chord(none, Key::parse("F8").unwrap()).is_some());
    }

    #[test]
    fn labels_are_readable() {
        assert_eq!(Trigger::default().label(), "Alt + Q");
        assert_eq!(
            "Ctrl+Oemtilde".parse::<Trigger>().unwrap().label(),
            "Ctrl + `"
        );
        assert_eq!("AltDouble".parse::<Trigger>().unwrap().label(), "双击 Alt");
    }

    #[test]
    fn double_tap_fires_once_per_pair() {
        let (ctrl, left_ctrl) = (0x11, 0xA2);
        let mut tap = DoubleTap::new(Modifier::Ctrl, 500);
        assert!(!tap.key(ctrl, true, 0));
        assert!(!tap.key(ctrl, true, 30));
        assert!(!tap.key(ctrl, false, 100));
        assert!(tap.key(left_ctrl, true, 300));
        assert!(!tap.key(ctrl, false, 350));
        assert!(!tap.key(ctrl, true, 400), "third tap must not fire again");
        assert!(!tap.key(ctrl, false, 450));
        assert!(!tap.key(ctrl, true, 1200), "too slow");
    }

    #[test]
    fn double_tap_ignores_chords() {
        let (ctrl, c) = (0x11, 0x43);
        let mut tap = DoubleTap::new(Modifier::Ctrl, 500);
        tap.key(ctrl, true, 0);
        tap.key(c, true, 10);
        tap.key(c, false, 20);
        tap.key(ctrl, false, 30);
        assert!(!tap.key(ctrl, true, 100));
        tap.key(ctrl, false, 120);
        tap.key(c, true, 130);
        assert!(!tap.key(ctrl, true, 140), "key in between cancels");
    }

    #[test]
    fn double_click_needs_proximity() {
        let mut click = DoubleClick::new(500, (2, 2));
        assert!(!click.press(0, 10, 10));
        assert!(click.press(200, 11, 9));
        assert!(!click.press(300, 11, 9), "triple click starts over");
        assert!(!click.press(400, 30, 9), "moved too far");
        assert!(!click.press(1000, 30, 9), "too slow");
    }

    #[test]
    fn every_key_parses_back() {
        for key in Key::all() {
            assert_eq!(Key::parse(&key.name()), Some(key), "{}", key.name());
        }
    }
}
