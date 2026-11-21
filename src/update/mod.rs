//! In-app updates from the GitHub releases of this repository.
//!
//! Flow: read `releases/latest`, compare the version, ask the user, download
//! the exe and its SHA-256 file, check both, then start the installed
//! widepeek (in the admin-only program folder) elevated with
//! `install-update`. That elevated step reads the SHA-256 of the release
//! from GitHub itself, checks a copy in the program folder and replaces the
//! installed exe. Nothing runs while a stretch session or the game runs.

mod apply;
mod http;
mod release;
mod stamp;
mod verify;

use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, bail};
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

pub use apply::apply_here;
pub use release::{Release, Version};

use crate::instance::{self, InstanceLock};
use crate::paths::{self, EXE_NAME};
use crate::{console, elevation, process, riot, ui};

/// Hidden command: the elevated install step. Arguments: the downloaded
/// exe, its version and the SID of the user.
pub const INSTALL_UPDATE: &str = "install-update";

const API_ACCEPT: &str = "application/vnd.github+json";
const API_MAX_BYTES: usize = 256 * 1024;
const SHA256_MAX_BYTES: usize = 4 * 1024;
pub(crate) const EXE_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Reads the latest release. Returns it if it is newer than this exe.
pub fn check() -> anyhow::Result<Option<Release>> {
    let url = release::latest_api_url()?;
    info!("checking for updates at {url}");
    let json = http::get(&url, API_ACCEPT, API_MAX_BYTES)?;
    stamp::mark_checked();
    let latest = release::parse(&json, release::repository_url())?;
    let current = Version::current();
    if latest.version > current {
        info!(
            "widepeek {} is available (this is {current})",
            latest.version
        );
        Ok(Some(latest))
    } else {
        info!("widepeek {current} is up to date");
        Ok(None)
    }
}

/// The automatic check: only if the user turned it on and the last check is
/// older than one day. Errors only go to the log.
pub fn check_if_due(enabled: bool) -> Option<Release> {
    if !enabled || !stamp::is_due() {
        return None;
    }
    check()
        .inspect_err(|err| warn!("update check failed: {err:#}"))
        .ok()
        .flatten()
}

/// `widepeek update`: check now, then offer the update.
pub fn command() -> anyhow::Result<()> {
    match check()? {
        Some(release) => {
            offer(&release)?;
        }
        None => tell(&format!("widepeek {} is up to date.", Version::current())),
    }
    Ok(())
}

/// Asks the user and installs the update on "Yes". Returns true if it was
/// installed.
pub fn offer(release: &Release) -> anyhow::Result<bool> {
    let question = format!(
        "widepeek {} is available. You have {}.\n\n\
         Install it now? Windows asks for administrator rights once.\n\n\
         Release notes: {}",
        release.version,
        Version::current(),
        release.page_url
    );
    if !ui::ask(&question) {
        info!("the user declined the update to {}", release.version);
        return Ok(false);
    }
    install(release)?;
    tell(&format!("widepeek {} is installed.", release.version));
    Ok(true)
}

fn tell(text: &str) {
    if console::has_console() {
        console::println(text);
    } else {
        ui::info(text);
    }
}

/// Downloads, checks and installs `release`.
pub fn install(release: &Release) -> anyhow::Result<()> {
    let _locks = quiet_period()?;
    let installed = paths::installed_exe()?;
    if !installed.exists() {
        bail!("widepeek is not installed. Run `widepeek setup` first.");
    }

    let expected = fetch_sha256(release)?;
    info!(
        "downloading {} (SHA-256 {})",
        release.exe_url,
        verify::to_hex(&expected)
    );
    let exe = http::get(&release.exe_url, "application/octet-stream", EXE_MAX_BYTES)?;
    if verify::sha256(&exe)? != expected {
        bail!("the downloaded exe does not match its SHA-256 file");
    }

    let dir = paths::update_dir()?;
    let download = dir.join(format!("widepeek-{}.exe", release.version));
    paths::write_atomic(&download, &exe)?;
    let result = install_download(&download, &installed, release.version);
    if let Err(err) = paths::remove_if_exists(&download) {
        warn!("{err:#}");
    }
    let _ = std::fs::remove_dir(&dir);
    result
}

fn install_download(download: &Path, installed: &Path, version: Version) -> anyhow::Result<()> {
    // Block writes while the checks and the elevated step run.
    let file = open_read_only(download)?;
    // Check the signature here too, so the user gets the error before the
    // administrator prompt. The elevated step checks it again.
    apply::check_signer(&file, download, &open_read_only(installed)?, installed)?;
    let sid = elevation::current_user_sid()?;
    let args = [
        INSTALL_UPDATE.to_owned(),
        download.display().to_string(),
        version.to_string(),
        sid,
    ];
    info!(
        "installing {} with administrator rights",
        download.display()
    );
    let code = elevation::run_elevated(installed, &args)?;
    drop(file);
    if code != 0 {
        bail!("the elevated update step failed (exit code {code}). See the log.");
    }
    info!("update installed");
    Ok(())
}

/// Reads the release `v<version>` of this repository.
fn fetch_release(version: Version) -> anyhow::Result<Release> {
    let json = http::get(&release::tag_api_url(version)?, API_ACCEPT, API_MAX_BYTES)?;
    let found = release::parse(&json, release::repository_url())?;
    if found.version != version {
        bail!("GitHub sent release {} for {version}", found.version);
    }
    Ok(found)
}

/// Reads the SHA-256 of `widepeek.exe` from the release.
fn fetch_sha256(release: &Release) -> anyhow::Result<verify::Sha256> {
    let file = http::get(
        &release.sha256_url,
        "application/octet-stream",
        SHA256_MAX_BYTES,
    )?;
    let text = std::str::from_utf8(&file).context("the SHA-256 file is not text")?;
    verify::parse_sha256_file(text, EXE_NAME)
}

/// Opens a file for reading and lets others only read it.
pub(crate) fn open_read_only(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))
}

/// Holds the launch and session locks, so no stretch session starts during
/// the install. Fails while widepeek stretches or the game runs.
fn quiet_period() -> anyhow::Result<(InstanceLock, InstanceLock)> {
    const BUSY: &str = "widepeek or Valorant is running. Close Valorant, then update.";
    let launch = instance::try_acquire(instance::LAUNCH)?.context(BUSY)?;
    let session = instance::try_acquire(instance::SESSION)?.context(BUSY)?;
    if process::find(riot::GAME_PROCESS).is_some()
        || process::find(riot::BOOTSTRAP_PROCESS).is_some()
    {
        bail!(BUSY);
    }
    Ok((launch, session))
}
