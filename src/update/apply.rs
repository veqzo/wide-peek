//! `install-update`: the elevated step of an update.
//!
//! It runs from `C:\Program Files\widepeek`, which only administrators can
//! change, so no planted DLL beside the download can load into it. Its
//! caller is unelevated, so it uses only the version and the path from it:
//! - the version must be higher than the installed one,
//! - the SHA-256 comes from that release of this repository on GitHub,
//! - the download is read once into a staging file in the program folder,
//!   and all checks run on that copy.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

use super::verify::{self, Signature};
use super::{EXE_MAX_BYTES, Version, fetch_release, fetch_sha256, open_read_only};
use crate::{cli, elevation, paths, system};

/// Checks that the new exe may replace the current one:
/// - a signature on the new exe must be valid,
/// - if the current exe is signed, the new exe must have the same subject.
///
/// One certificate can sign many projects (SignPath Foundation), so this is
/// not the main check. The main check is the SHA-256 of the release.
pub fn check_signer(
    new_file: &File,
    new_path: &Path,
    own_file: &File,
    own_path: &Path,
) -> anyhow::Result<()> {
    let new = verify::signature(new_file, new_path)?;
    let own = verify::signature(own_file, own_path)?;
    info!("signature of the update: {new:?}, of the installed exe: {own:?}");
    match (own, new) {
        (Signature::Valid { subject: own }, Signature::Valid { subject: new }) if own == new => {
            Ok(())
        }
        (Signature::Valid { subject: own }, Signature::Valid { subject: new }) => {
            bail!("the update is signed by `{new}`, not by `{own}`")
        }
        (Signature::Valid { subject }, Signature::Unsigned) => {
            bail!("the update is not signed. The installed widepeek is signed by `{subject}`.")
        }
        (Signature::Unsigned, _) => Ok(()),
    }
}

/// `install-update <exe> <version> <user sid>`
pub fn apply_here(download: &Path, version: &str, user_sid: &str) -> anyhow::Result<()> {
    let own_sid = elevation::current_user_sid()?;
    if !own_sid.eq_ignore_ascii_case(user_sid) {
        bail!("the administrator prompt used another Windows account");
    }
    let installed = paths::installed_exe()?;
    if !system::same_file(&elevation::current_exe()?, &installed) {
        bail!("only the installed widepeek can install an update");
    }
    let version = Version::parse(version)?;
    let current = Version::current();
    if version <= current {
        bail!("widepeek {version} is not newer than the installed {current}");
    }
    let expected = fetch_sha256(&fetch_release(version)?)?;

    let image = read_download(download)?;
    let staged = system::write_staging(&image)?;
    drop(image);
    let checked = check_staged(&staged, &installed, &expected);
    if let Err(err) = checked {
        let _ = std::fs::remove_file(&staged);
        return Err(err);
    }
    system::install_staged(&staged)?;
    info!("widepeek {version} is in place");

    // The new version writes its own scheduled tasks. It inherits the
    // administrator rights and runs from the program folder.
    let status = Command::new(&installed)
        .args([cli::INSTALL_SYSTEM, user_sid])
        .status()
        .with_context(|| format!("cannot start {}", installed.display()))?;
    if !status.success() {
        bail!("the new widepeek could not update its tasks ({status})");
    }
    Ok(())
}

/// Reads the download once. It must be a file in the update folder of this
/// user on a local disk, so no remote server can send other bytes on a
/// second read.
fn read_download(download: &Path) -> anyhow::Result<Vec<u8>> {
    let dir = paths::update_dir()?;
    let in_update_dir = download
        .parent()
        .is_some_and(|p| p.as_os_str().eq_ignore_ascii_case(dir.as_os_str()));
    if !in_update_dir {
        bail!("the update file is not in {}", dir.display());
    }
    let file = open_read_only(download)?;
    let real = system::final_dos_path(&file)?;
    if !is_local_drive_path(&real) {
        bail!("the update file is not on a local disk ({real})");
    }
    read_limited(&file, download)
}

/// True for `\\?\C:\...`, false for `\\?\UNC\...` and other devices.
fn is_local_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(r"\\?\")
        && bytes.len() > 6
        && bytes[4].is_ascii_alphabetic()
        && bytes[5] == b':'
        && bytes[6] == b'\\'
}

/// Checks the staged copy: hash, exe header and signer.
fn check_staged(staged: &Path, installed: &Path, expected: &verify::Sha256) -> anyhow::Result<()> {
    let file = open_read_only(staged)?;
    let image = read_limited(&file, staged)?;
    if verify::sha256(&image)? != *expected {
        bail!("the update file does not match the SHA-256 of the release");
    }
    if !image.starts_with(b"MZ") {
        bail!("the update file is not a Windows program");
    }
    check_signer(&file, staged, &open_read_only(installed)?, installed)
}

fn read_limited(file: &File, path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut image = Vec::new();
    file.take(EXE_MAX_BYTES as u64 + 1)
        .read_to_end(&mut image)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if image.len() > EXE_MAX_BYTES {
        bail!("the update file is larger than {EXE_MAX_BYTES} bytes");
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_local_drive_paths() {
        assert!(is_local_drive_path(r"\\?\C:\Users\me\x.exe"));
        assert!(!is_local_drive_path(r"\\?\UNC\server\share\x.exe"));
        assert!(!is_local_drive_path(r"\\?\GLOBALROOT\Device\x.exe"));
        assert!(!is_local_drive_path(r"C:\Users\me\x.exe"));
        assert!(!is_local_drive_path(r"\\?\C:"));
    }
}
