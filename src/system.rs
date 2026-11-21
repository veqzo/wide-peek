//! The elevated parts of setup and uninstall.
//!
//! They only touch admin-only places: `C:\Program Files\widepeek` and the
//! `\widepeek` task folder. They never read or write the user-writable
//! config, state or log, so a medium-integrity process cannot redirect an
//! elevated write with a planted link.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FILE_SHARE_READ, GETFINALPATHNAMEBYHANDLE_FLAGS, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    VOLUME_NAME_NT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::GetMappedFileNameW;
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::monitor::tasks;
use crate::{cli, elevation, paths};

/// Runs the elevated part of setup: in this process if it is elevated,
/// otherwise in an elevated copy. Returns the installed exe.
pub fn install(user_sid: &str) -> anyhow::Result<PathBuf> {
    if elevation::is_elevated() {
        install_here(user_sid)
    } else {
        run_elevated(
            &[cli::INSTALL_SYSTEM.to_owned(), user_sid.to_owned()],
            "setup",
        )?;
        paths::installed_exe()
    }
}

/// Runs the elevated part of uninstall, like `install`.
pub fn uninstall() -> anyhow::Result<()> {
    if elevation::is_elevated() {
        uninstall_here()
    } else {
        run_elevated(&[cli::UNINSTALL_SYSTEM.to_owned()], "uninstall")
    }
}

fn run_elevated(args: &[String], what: &str) -> anyhow::Result<()> {
    let exe = elevation::current_exe()?;
    let code = elevation::run_elevated(&exe, args)?;
    if code != 0 {
        bail!("the elevated {what} step failed (exit code {code})");
    }
    Ok(())
}

/// `install-system`: copies the exe and creates the tasks.
pub fn install_here(user_sid: &str) -> anyhow::Result<PathBuf> {
    // An administrator prompt with another account would create the tasks
    // for that account. The launcher of this user could not use them.
    let own_sid = elevation::current_user_sid()?;
    if !own_sid.eq_ignore_ascii_case(user_sid) {
        bail!(
            "the administrator prompt used another Windows account. Run setup from an \
             account with administrator rights."
        );
    }
    let exe = install_exe()?;
    tasks::install(&exe, user_sid)?;
    Ok(exe)
}

/// `uninstall-system`: deletes the tasks and the program folder.
pub fn uninstall_here() -> anyhow::Result<()> {
    tasks::uninstall()?;
    remove_install_dir()
}

/// Copies the running image to `C:\Program Files\widepeek`. A running copy
/// there cannot be replaced, so it is renamed and deleted at the next reboot.
fn install_exe() -> anyhow::Result<PathBuf> {
    let target = paths::installed_exe()?;
    if same_file(&elevation::current_exe()?, &target) {
        return Ok(target);
    }
    replace_installed(&read_running_image()?)
}

/// Writes `image` to `C:\Program Files\widepeek\widepeek.exe` through a
/// staging file, so a failed write never leaves a broken exe.
fn replace_installed(image: &[u8]) -> anyhow::Result<PathBuf> {
    install_staged(&write_staging(image)?)
}

/// `C:\Program Files\widepeek\widepeek.new.exe`. Only administrators can
/// write there, so its content stays what the elevated step wrote.
fn staging_exe() -> anyhow::Result<PathBuf> {
    Ok(paths::install_dir()?.join("widepeek.new.exe"))
}

/// Writes `image` to a new staging file and flushes it to the disk.
pub fn write_staging(image: &[u8]) -> anyhow::Result<PathBuf> {
    let dir = paths::install_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let staged = staging_exe()?;
    paths::remove_if_exists(&staged)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .with_context(|| format!("cannot create {}", staged.display()))?;
    let written = file.write_all(image).and_then(|()| file.sync_all());
    drop(file);
    if let Err(err) = written {
        let _ = fs::remove_file(&staged);
        return Err(err).with_context(|| format!("cannot write {}", staged.display()));
    }
    Ok(staged)
}

/// Moves the staging file to the installed exe. A running exe cannot be
/// deleted, so the old exe gets a unique name first and is deleted later.
/// If the last move fails, the old exe goes back to its place.
pub fn install_staged(staged: &Path) -> anyhow::Result<PathBuf> {
    let target = paths::installed_exe()?;
    remove_old_copies();
    let old = if target.exists() {
        let old = target.with_extension(format!("old-{}.exe", std::process::id()));
        fs::rename(&target, &old).with_context(|| {
            format!(
                "cannot replace {}. Close widepeek and try again.",
                target.display()
            )
        })?;
        Some(old)
    } else {
        None
    };
    if let Err(err) = fs::rename(staged, &target) {
        if let Some(old) = &old
            && let Err(back) = fs::rename(old, &target)
        {
            error!("cannot put back {}: {back}", target.display());
        }
        let _ = fs::remove_file(staged);
        return Err(err).with_context(|| format!("cannot install {}", target.display()));
    }
    if let Some(old) = old
        && fs::remove_file(&old).is_err()
    {
        paths::delete_at_reboot(&old);
    }
    Ok(target)
}

/// Deletes the old exes of earlier updates that no process runs any more.
fn remove_old_copies() {
    let Ok(entries) = paths::install_dir().and_then(|dir| Ok(fs::read_dir(dir)?)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("widepeek.old") && name.ends_with(".exe") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Reads the file that this process runs from. The path of the exe is not
/// enough: a medium-integrity process can rename the running exe and put
/// another file at its path. So this opens the mapped image by its current
/// NT path, blocks writes and renames, and checks that the handle is still
/// the mapped image.
fn read_running_image() -> anyhow::Result<Vec<u8>> {
    let mapped = mapped_image_path()?;
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(format!(r"\\?\GLOBALROOT{mapped}"))
        .with_context(|| format!("cannot open the running image {mapped}"))?;
    let opened = final_nt_path(&file)?;
    if !opened.eq_ignore_ascii_case(&mapped_image_path()?) {
        bail!("the running image changed during setup");
    }
    let mut image = Vec::new();
    file.read_to_end(&mut image)
        .context("cannot read the running image")?;
    Ok(image)
}

/// NT path of the main image, for example
/// `\Device\HarddiskVolume3\Users\me\Downloads\widepeek.exe`.
fn mapped_image_path() -> anyhow::Result<String> {
    // SAFETY: the module handle of the exe is its base address and stays
    // valid for the life of the process.
    let base = unsafe { GetModuleHandleW(None) }.context("cannot find the exe module")?;
    let mut buf = vec![0u16; 32_768];
    // SAFETY: `base` is a mapped address of this process; the buffer is valid.
    let len = unsafe { GetMappedFileNameW(GetCurrentProcess(), base.0, &mut buf) } as usize;
    if len == 0 || len >= buf.len() {
        bail!(
            "cannot read the path of the running image: {}",
            std::io::Error::last_os_error()
        );
    }
    String::from_utf16(&buf[..len]).context("the image path is not valid UTF-16")
}

fn final_nt_path(file: &File) -> anyhow::Result<String> {
    final_path(file, VOLUME_NAME_NT)
}

/// Win32 path of an open file after all links, for example
/// `\\?\C:\Users\me\x.exe` or `\\?\UNC\server\share\x.exe`.
pub fn final_dos_path(file: &File) -> anyhow::Result<String> {
    final_path(file, VOLUME_NAME_DOS)
}

fn final_path(file: &File, flags: GETFINALPATHNAMEBYHANDLE_FLAGS) -> anyhow::Result<String> {
    let mut buf = vec![0u16; 32_768];
    // SAFETY: the handle belongs to `file`, which lives during the call.
    let len = unsafe { GetFinalPathNameByHandleW(HANDLE(file.as_raw_handle()), &mut buf, flags) }
        as usize;
    if len == 0 || len >= buf.len() {
        bail!(
            "cannot read the path of an opened file: {}",
            std::io::Error::last_os_error()
        );
    }
    String::from_utf16(&buf[..len]).context("the file path is not valid UTF-16")
}

pub fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Deletes `C:\Program Files\widepeek`. A running exe there (for example
/// the tray) is deleted at the next reboot.
fn remove_install_dir() -> anyhow::Result<()> {
    let dir = paths::install_dir()?;
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(());
    };
    let mut pending = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if fs::remove_file(&path).is_err() {
            paths::delete_at_reboot(&path);
            pending = true;
        }
    }
    if pending {
        paths::delete_at_reboot(&dir);
    } else {
        fs::remove_dir(&dir).with_context(|| format!("cannot delete {}", dir.display()))?;
    }
    Ok(())
}
