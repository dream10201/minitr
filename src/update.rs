use std::fmt;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::{env, fs};

use anyhow::{Context as _, Result, bail};
use reqwest::Client;
use serde::Deserialize;

const LATEST_RELEASE: &str = "https://api.github.com/repos/dream10201/minitr/releases/latest";
const EXE_NAME: &str = "MiniTranslation.exe";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([u32; 4]);

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let text = text.split(['-', '+']).next()?;
        let mut parts = [0; 4];
        let mut count = 0;
        for part in text.split('.') {
            *parts.get_mut(count)? = part.parse().ok()?;
            count += 1;
        }
        (count >= 2).then_some(Self(parts))
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("package version is numeric")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch, revision] = self.0;
        write!(f, "{major}.{minor}.{patch}")?;
        if revision != 0 {
            write!(f, ".{revision}")?;
        }
        Ok(())
    }
}

/// A newer version already unpacked next to the running one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub version: Version,
    pub exe: PathBuf,
}

pub fn enabled() -> bool {
    !cfg!(debug_assertions)
}

/// `%LocalAppData%\MiniTranslation\app\<version>\MiniTranslation.exe`
fn versions_dir() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map_or_else(env::temp_dir, PathBuf::from)
        .join("MiniTranslation")
        .join("app")
}

static BASE_EXE: OnceLock<PathBuf> = OnceLock::new();

/// Records the installed executable this process was started on behalf of.
pub fn init_base(args: &[String]) {
    if let Some(path) = args
        .iter()
        .position(|a| a == "--base")
        .and_then(|i| args.get(i + 1))
    {
        let _ = BASE_EXE.set(PathBuf::from(path));
    }
}

/// The installed executable, even when running from a version directory.
pub fn base_exe() -> PathBuf {
    BASE_EXE
        .get()
        .cloned()
        .or_else(|| env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(EXE_NAME))
}

pub fn pending() -> Option<Pending> {
    let current = Version::current();
    fs::read_dir(versions_dir())
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let version = Version::parse(entry.file_name().to_str()?)?;
            let exe = entry.path().join(EXE_NAME);
            (version > current && exe.is_file()).then_some(Pending { version, exe })
        })
        .max_by_key(|pending| pending.version)
}

/// The caller should exit afterwards; the new process waits for the instance lock.
pub fn launch(pending: &Pending) -> io::Result<()> {
    Command::new(&pending.exe)
        .arg("--base")
        .arg(base_exe())
        .spawn()
        .map(drop)
}

/// Hands over to a newer unpacked version if there is one; returns whether it did.
pub fn launch_newer_at_startup() -> bool {
    if !enabled() {
        return false;
    }
    match pending() {
        Some(pending) => launch(&pending).is_ok(),
        None => {
            remove_stale_versions();
            false
        }
    }
}

fn remove_stale_versions() {
    let Ok(entries) = fs::read_dir(versions_dir()) else {
        return;
    };
    let own_dir = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let current = Version::current();
    for entry in entries.flatten() {
        let path = entry.path();
        let newer = entry
            .file_name()
            .to_str()
            .and_then(Version::parse)
            .is_some_and(|v| v > current);
        if !newer && own_dir.as_deref() != Some(path.as_path()) {
            let _ = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
        }
    }
}

/// Run elevated at logon by the installer's task: copies the newest unpacked version
/// over the installed executable. Only copies, never executes user-writable files.
pub fn sync_base() {
    let Ok(base) = env::current_exe() else { return };
    let retired = base.with_extension("exe.old");
    let _ = fs::remove_file(&retired);
    let Some(pending) = pending() else { return };
    // A running executable cannot be overwritten but can be renamed.
    if fs::rename(&base, &retired).is_ok() && fs::copy(&pending.exe, &base).is_err() {
        let _ = fs::rename(&retired, &base);
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Downloads and unpacks a newer release if one exists. `on_progress` gets percentages.
pub async fn check_and_download(
    client: &Client,
    mut on_progress: impl FnMut(u8),
) -> Result<Option<Pending>> {
    if let Some(pending) = pending() {
        return Ok(Some(pending));
    }
    let body = client
        .get(LATEST_RELEASE)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let release: Release = serde_json::from_slice(&body)?;
    let Some(version) = Version::parse(&release.tag_name).filter(|v| *v > Version::current())
    else {
        return Ok(None);
    };
    let Some(asset) = release
        .assets
        .iter()
        .find(|a| a.name.to_ascii_lowercase().ends_with(".zip"))
    else {
        return Ok(None);
    };

    let mut response = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?;
    let total = response.content_length().filter(|&n| n > 0);
    let mut archive = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut reported = None;
    while let Some(chunk) = response.chunk().await? {
        archive.extend_from_slice(&chunk);
        if let Some(total) = total {
            let percent = (archive.len() as u64 * 100 / total).min(100) as u8;
            if reported != Some(percent) {
                reported = Some(percent);
                on_progress(percent);
            }
        }
    }

    let exe = tokio::task::spawn_blocking(move || unpack(&archive, version)).await??;
    Ok(Some(Pending { version, exe }))
}

/// Unpacks into a staging directory first so a half-written version is never picked up.
fn unpack(archive: &[u8], version: Version) -> Result<PathBuf> {
    let root = versions_dir();
    let staging = root.join("staging");
    let _ = fs::remove_dir_all(&staging);
    zip::ZipArchive::new(Cursor::new(archive))
        .context("更新包已损坏")?
        .extract(&staging)
        .context("无法解压更新包")?;
    if !staging.join(EXE_NAME).is_file() {
        bail!("更新包中缺少 {EXE_NAME}");
    }
    let target = root.join(version.to_string());
    let _ = fs::remove_dir_all(&target);
    fs::rename(&staging, &target)?;
    Ok(target.join(EXE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_versions() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("v3.0.0") > v("2.9.9"));
        assert!(v("2.10") > v("2.9.1"));
        assert_eq!(v("V3.1.2-beta+abc").to_string(), "3.1.2");
        assert_eq!(v("1.2.3.4").to_string(), "1.2.3.4");
        assert!(Version::parse("staging").is_none());
        assert!(Version::parse("1").is_none());
        assert!(Version::parse("1.2.3.4.5").is_none());
    }

    #[test]
    fn current_version_is_newer_than_dotnet_line() {
        assert!(Version::current() > Version::parse("2.0.0").unwrap());
    }
}
