use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::{Deserialize, Serialize};

use crate::hotkey::Trigger;
use crate::translate::Language;

/// One OpenAI-compatible endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub api_base_url: String,
    pub api_key: String,
    pub model: String,
}

impl Profile {
    pub fn is_complete(&self) -> bool {
        [&self.api_base_url, &self.api_key, &self.model]
            .iter()
            .all(|field| !field.trim().is_empty())
    }

    pub fn is_blank(&self) -> bool {
        [&self.api_base_url, &self.api_key, &self.model]
            .iter()
            .all(|field| field.trim().is_empty())
    }

    pub fn display_name(&self) -> String {
        if self.model.trim().is_empty() && self.api_base_url.trim().is_empty() {
            return "（未填写）".into();
        }
        format!("{} @ {}", self.model, host_of(&self.api_base_url))
    }
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    match host.strip_prefix('[') {
        Some(ipv6) => ipv6.split(']').next().unwrap_or(ipv6),
        None => host.split(':').next().unwrap_or(host),
    }
}

/// Machine-local data: unpacked updates and the log.
pub fn local_data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("MiniTranslation")
}

/// A way of reading the text the user means when the window is summoned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Capture {
    Automation,
    CtrlInsert,
    CtrlC,
    Clipboard,
}

impl Capture {
    pub const ALL: [Self; 4] = [
        Self::Automation,
        Self::CtrlInsert,
        Self::CtrlC,
        Self::Clipboard,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Automation => "系统接口读取选区",
            Self::CtrlInsert => "模拟 Ctrl+Insert 复制选区",
            Self::CtrlC => "模拟 Ctrl+C 复制选区",
            Self::Clipboard => "读取剪贴板新内容",
        }
    }

    /// Reads from the app in front, so it only works before this window takes the focus.
    pub fn needs_target(self) -> bool {
        self != Self::Clipboard
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStep {
    pub method: Capture,
    pub enabled: bool,
}

/// Every method exactly once, in the stored order. Without a stored list the
/// switches of earlier releases decide.
fn capture_steps(value: &serde_json::Value) -> Vec<CaptureStep> {
    let mut steps: Vec<CaptureStep> = match value.get("capture").and_then(|v| v.as_array()) {
        Some(items) => items
            .iter()
            .filter_map(|item| CaptureStep::deserialize(item).ok())
            .collect(),
        None => {
            let flag = |key: &str, default: bool| {
                value.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
            };
            let selection = flag("autoTranslateSelection", false);
            let clipboard = flag("autoTranslateClipboard", true);
            Capture::ALL
                .map(|method| CaptureStep {
                    method,
                    enabled: match method {
                        Capture::Automation | Capture::CtrlInsert => selection,
                        Capture::CtrlC => false,
                        Capture::Clipboard => clipboard,
                    },
                })
                .to_vec()
        }
    };
    let mut seen = Vec::new();
    steps.retain(|step| {
        let first = !seen.contains(&step.method);
        seen.push(step.method);
        first
    });
    for method in Capture::ALL {
        if !seen.contains(&method) {
            steps.push(CaptureStep {
                method,
                enabled: false,
            });
        }
    }
    steps
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Ordered by the user; earlier profiles are tried first.
    pub profiles: Vec<Profile>,
    /// Ordered by the user; enabled methods are tried until one yields text.
    #[serde(skip_deserializing)]
    pub capture: Vec<CaptureStep>,
    pub hot_key: Trigger,
    pub double_press_interval_ms: u32,
    pub foreign_language: Language,
    pub hide_on_focus_lost: bool,
    pub auto_check_update: bool,
    pub auto_copy_result: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profiles: Vec::new(),
            capture: capture_steps(&serde_json::Value::Null),
            hot_key: Trigger::default(),
            double_press_interval_ms: 500,
            foreign_language: Language::English,
            hide_on_focus_lost: false,
            auto_check_update: true,
            auto_copy_result: false,
        }
    }
}

pub const INTERVAL_RANGE: std::ops::RangeInclusive<u32> = 100..=2000;

impl Settings {
    pub fn is_configured(&self) -> bool {
        self.profiles.iter().any(Profile::is_complete)
    }

    pub fn captures(&self) -> impl Iterator<Item = Capture> + '_ {
        self.capture
            .iter()
            .filter(|step| step.enabled)
            .map(|step| step.method)
    }

    pub fn double_press_interval(&self) -> std::time::Duration {
        let ms = self
            .double_press_interval_ms
            .clamp(*INTERVAL_RANGE.start(), *INTERVAL_RANGE.end());
        std::time::Duration::from_millis(ms.into())
    }

    /// Roaming: follows the user across machines.
    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
        base.join("MiniTranslation").join("settings.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::path())
    }

    /// A missing file yields defaults; an unreadable one is set aside rather than
    /// silently overwritten by the next save.
    pub fn load_from(path: &Path) -> Self {
        let Ok(text) = fs::read_to_string(path) else {
            return Self::default();
        };
        match Self::parse(&text) {
            Some(settings) => settings,
            None => {
                let _ = fs::rename(path, path.with_extension("json.bad"));
                Self::default()
            }
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let mut settings: Self = serde_json::from_value(value.clone()).ok()?;
        settings.capture = capture_steps(&value);
        if settings.profiles.is_empty() {
            // Releases before multi-profile support stored a single endpoint at the top level.
            let legacy: Profile = serde_json::from_value(value).unwrap_or_default();
            if legacy.is_complete() {
                settings.profiles.push(legacy);
            }
        }
        Some(settings)
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let staging = path.with_extension("json.tmp");
        fs::write(&staging, json)?;
        fs::rename(&staging, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_files_written_by_the_dotnet_release() {
        let json = r#"{
            "profiles": [{ "apiBaseUrl": "https://api.deepseek.com", "apiKey": "k", "model": "deepseek-chat" }],
            "autoTranslateClipboard": false,
            "autoTranslateSelection": true,
            "hotKey": "CtrlDouble",
            "doublePressIntervalMs": 300,
            "hideOnFocusLost": true,
            "autoCheckUpdate": false,
            "autoCopyResult": true
        }"#;
        let settings = Settings::parse(json).unwrap();
        assert_eq!(settings.profiles[0].model, "deepseek-chat");
        assert_eq!(
            settings.captures().collect::<Vec<_>>(),
            [Capture::Automation, Capture::CtrlInsert]
        );
        assert_eq!(settings.hot_key.to_string(), "CtrlDouble");
        assert_eq!(settings.double_press_interval_ms, 300);
        assert!(settings.is_configured());
    }

    #[test]
    fn migrates_single_profile_layout() {
        let json =
            r#"{ "apiBaseUrl": "http://localhost:11434/v1", "apiKey": "x", "model": "qwen3" }"#;
        let settings = Settings::parse(json).unwrap();
        assert_eq!(settings.profiles.len(), 1);
        assert_eq!(settings.profiles[0].display_name(), "qwen3 @ localhost");
    }

    #[test]
    fn capture_list_is_completed_and_deduplicated() {
        let json = r#"{ "capture": [
            { "method": "clipboard", "enabled": true },
            { "method": "future", "enabled": true },
            { "method": "ctrlC", "enabled": true },
            { "method": "clipboard", "enabled": false }
        ] }"#;
        let settings = Settings::parse(json).unwrap();
        let order: Vec<_> = settings.capture.iter().map(|step| step.method).collect();
        assert_eq!(
            order,
            [
                Capture::Clipboard,
                Capture::CtrlC,
                Capture::Automation,
                Capture::CtrlInsert
            ]
        );
        assert_eq!(
            settings.captures().collect::<Vec<_>>(),
            [Capture::Clipboard, Capture::CtrlC]
        );
    }

    #[test]
    fn unknown_hotkey_falls_back_to_default() {
        let settings = Settings::parse(r#"{ "hotKey": "Nonsense+" }"#).unwrap();
        assert_eq!(settings.hot_key, Trigger::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("minits-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut settings = Settings::default();
        settings.profiles.push(Profile {
            api_base_url: "https://api.openai.com/v1".into(),
            api_key: "sk".into(),
            model: "gpt-4o-mini".into(),
        });
        settings.capture.swap(0, 3);
        settings.capture[1].enabled = true;
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), settings);
        fs::write(&path, "{ broken").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
        assert!(path.with_extension("json.bad").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn host_extraction() {
        assert_eq!(
            host_of("https://user:pw@api.example.com:8443/v1"),
            "api.example.com"
        );
        assert_eq!(host_of("api.example.com"), "api.example.com");
        assert_eq!(host_of("http://[::1]:8080/v1"), "::1");
    }
}
