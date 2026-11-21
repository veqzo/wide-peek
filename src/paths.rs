//! File system locations and an atomic file write.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_ProgramData, FOLDERID_ProgramFiles,
    FOLDERID_Programs, FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, HSTRING, PCWSTR};

pub const APP_DIR: &str = "widepeek";
pub const EXE_NAME: &str = "widepeek.exe";

fn known_folder(id: &GUID, env_fallback: &str) -> anyhow::Result<PathBuf> {
    // SAFETY: valid GUID pointer; the returned buffer is freed below.
    if let Ok(ptr) = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) } {
        // SAFETY: `ptr` is a NUL-terminated string from the shell.
        let path = unsafe { ptr.to_string() };
        // SAFETY: the shell allocated `ptr` with CoTaskMemAlloc.
        unsafe { CoTaskMemFree(Some(ptr.0 as *const _)) };
        if let Ok(path) = path
            && !path.is_empty()
        {
            return Ok(PathBuf::from(path));
        }
    }
    std::env::var_os(env_fallback)
        .map(PathBuf::from)
        .with_context(|| format!("cannot find the folder for %{env_fallback}%"))
}

/// `%AppData%\widepeek`
pub fn config_dir() -> anyhow::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_RoamingAppData, "APPDATA")?.join(APP_DIR))
}

/// `%LocalAppData%\widepeek`
pub fn data_dir() -> anyhow::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_LocalAppData, "LOCALAPPDATA")?.join(APP_DIR))
}

pub fn config_file() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn state_file() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("state.json"))
}

/// Time of the last update check.
pub fn update_stamp_file() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("update.json"))
}

/// Downloaded updates wait here until they are installed.
pub fn update_dir() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("update"))
}

pub fn log_file() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("widepeek.log"))
}

/// `C:\Program Files\widepeek`. Only administrators can write here, so the
/// elevated scheduled tasks can safely point to this location.
pub fn install_dir() -> anyhow::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_ProgramFiles, "ProgramFiles")?.join(APP_DIR))
}

pub fn installed_exe() -> anyhow::Result<PathBuf> {
    Ok(install_dir()?.join(EXE_NAME))
}

pub fn program_data() -> anyhow::Result<PathBuf> {
    known_folder(&FOLDERID_ProgramData, "ProgramData")
}

/// The Start menu `Programs` folder of the current user.
pub fn start_menu_programs() -> anyhow::Result<PathBuf> {
    known_folder(&FOLDERID_Programs, "APPDATA")
}

pub fn desktop() -> anyhow::Result<PathBuf> {
    known_folder(&FOLDERID_Desktop, "USERPROFILE")
}

/// Writes a file so that readers see either the old or the new content:
/// write a temporary file, flush it to disk, then rename it.
pub fn write_atomic(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().context("path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let tmp = path.with_extension("tmp");
    {
        let mut file =
            File::create(&tmp).with_context(|| format!("cannot write {}", tmp.display()))?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

/// Deletes a file. A missing file is not an error.
pub fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot delete {}", path.display())),
    }
}

/// Marks a file or an empty folder for deletion at the next reboot.
pub fn delete_at_reboot(path: &Path) {
    // SAFETY: valid path string; NULL as the new name means "delete".
    let result = unsafe {
        MoveFileExW(
            &HSTRING::from(path),
            PCWSTR::null(),
            MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    };
    match result {
        Ok(()) => info!("{} is deleted at the next reboot", path.display()),
        Err(err) => warn!("cannot schedule the deletion of {}: {err}", path.display()),
    }
}
