use std::fmt;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
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

/// A downloaded release waiting to be installed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub version: Version,
    exe: PathBuf,
}

pub fn enabled() -> bool {
    !cfg!(debug_assertions)
}

/// `%LocalAppData%\MiniTranslation\update\<version>\MiniTranslation.exe`
fn downloads_dir() -> PathBuf {
    crate::settings::local_data_dir().join("update")
}

pub fn pending() -> Option<Pending> {
    let current = Version::current();
    fs::read_dir(downloads_dir())
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let version = Version::parse(entry.file_name().to_str()?)?;
            let exe = entry.path().join(EXE_NAME);
            (version > current && exe.is_file()).then_some(Pending { version, exe })
        })
        .max_by_key(|pending| pending.version)
}

fn retired(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// Removes what an earlier update left behind: the replaced executable and
/// downloads that are not newer than the running version.
pub fn clean_up() {
    if let Ok(exe) = env::current_exe() {
        let _ = fs::remove_file(retired(&exe));
    }
    let downloads = pending().map(|pending| downloads_dir().join(pending.version.to_string()));
    let Ok(entries) = fs::read_dir(downloads_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if downloads.as_deref() != Some(entry.path().as_path()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Puts `source` in place of `target`. A running executable cannot be overwritten
/// but can be renamed, so the old one moves aside and is deleted on next start.
fn replace(source: &Path, target: &Path) -> io::Result<()> {
    let old = retired(target);
    let _ = fs::remove_file(&old);
    fs::rename(target, &old)?;
    fs::copy(source, target).map(drop).inspect_err(|_| {
        let _ = fs::rename(&old, target);
    })
}

/// Installs `pending` over the running executable, asking for administrator
/// rights when its directory is not writable. Blocks until done and returns the
/// executable to start.
pub fn install(pending: &Pending) -> Result<PathBuf> {
    let target = env::current_exe()?;
    match replace(&pending.exe, &target) {
        Ok(()) => Ok(target),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            install_elevated(&target, &pending.exe)?;
            Ok(target)
        }
        Err(error) => Err(error).context("无法替换程序文件"),
    }
}

/// Runs the installed, administrator-protected copy of this program with
/// `--install`; the downloaded file is only copied, never executed elevated.
#[cfg(windows)]
fn install_elevated(target: &Path, source: &Path) -> Result<()> {
    let parameters = format!("--install \"{}\"", source.display());
    match crate::platform::elevate::run_as_admin(target, &parameters)? {
        0 => Ok(()),
        code => bail!("安装更新失败（代码 {code}）"),
    }
}

#[cfg(not(windows))]
fn install_elevated(_: &Path, _: &Path) -> Result<()> {
    bail!("无权写入安装目录")
}

/// Entry point of the elevated `--install <source>` process; returns the exit code.
pub fn install_here(source: &Path) -> i32 {
    match env::current_exe().and_then(|target| replace(source, &target)) {
        Ok(()) => 0,
        Err(error) => {
            log::error!("installing {} failed: {error}", source.display());
            1
        }
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
    /// `sha256:<hex>`, published by GitHub for every asset.
    digest: Option<String>,
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

    let expected = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"));
    if expected.is_some_and(|hash| !hash.eq_ignore_ascii_case(&sha256_hex(&archive))) {
        bail!("更新包校验失败");
    }
    let exe = tokio::task::spawn_blocking(move || unpack(&archive, version)).await??;
    Ok(Some(Pending { version, exe }))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Unpacks into a staging directory first so a half-written version is never picked up.
fn unpack(archive: &[u8], version: Version) -> Result<PathBuf> {
    let root = downloads_dir();
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
    fn replace_keeps_the_old_file_aside() {
        let dir = std::env::temp_dir().join(format!("minits-replace-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (source, target) = (dir.join("new.exe"), dir.join("app.exe"));
        fs::write(&source, "new").unwrap();
        fs::write(&target, "old").unwrap();
        replace(&source, &target).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::read_to_string(retired(&target)).unwrap(), "old");
        replace(&source, &dir.join("missing.exe")).unwrap_err();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hashes_as_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn current_version_is_newer_than_dotnet_line() {
        assert!(Version::current() > Version::parse("2.0.0").unwrap());
    }
}
