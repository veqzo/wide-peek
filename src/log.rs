//! Small file logger: `%LocalAppData%\widepeek\widepeek.log`, rotated to
//! `widepeek.log.1` at about 1 MB. Each line has a local timestamp, the
//! process id and the command.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

struct Logger {
    tag: &'static str,
    pid: u32,
    file: Option<(PathBuf, File)>,
}

static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write("INFO", format_args!($($arg)*)) };
}

macro_rules! warn {
    ($($arg:tt)*) => { $crate::log::write("WARN", format_args!($($arg)*)) };
}

macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write("ERROR", format_args!($($arg)*)) };
}

/// Starts the logger. Without `to_file`, log calls do nothing.
pub fn init(tag: &'static str, to_file: bool) {
    // A missing log must never stop the tool, so an open error only means
    // "no file logging".
    let file = if to_file { open_log().ok() } else { None };
    *lock() = Some(Logger {
        tag,
        pid: std::process::id(),
        file,
    });
}

/// Closes the log file, so that uninstall can delete the folder.
pub fn close() {
    if let Some(logger) = lock().as_mut() {
        logger.file = None;
    }
}

pub fn write(level: &str, args: fmt::Arguments<'_>) {
    let mut guard = lock();
    let Some(logger) = guard.as_mut() else {
        return;
    };
    let Some((path, file)) = logger.file.as_mut() else {
        return;
    };
    // SAFETY: GetLocalTime has no preconditions.
    let t = unsafe { GetLocalTime() };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} [{} {}] {:<5} {}\r\n",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        t.wMilliseconds,
        logger.pid,
        logger.tag,
        level,
        args
    );
    if file.metadata().is_ok_and(|m| m.len() >= MAX_LOG_BYTES)
        && let Ok(rotated) = rotate(path)
    {
        *file = rotated;
    }
    // A failed log write cannot be reported anywhere else.
    let _ = file.write_all(line.as_bytes());
}

fn lock() -> MutexGuard<'static, Option<Logger>> {
    LOGGER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn open_log() -> anyhow::Result<(PathBuf, File)> {
    let path = crate::paths::log_file()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = open_append(&path)?;
    Ok((path, file))
}

/// Opens the log for appending. It refuses links, so that the log cannot be
/// redirected to another file.
fn open_append(path: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        anyhow::bail!("log file is a reparse point");
    }
    Ok(file)
}

fn rotate(path: &Path) -> anyhow::Result<File> {
    let old = path.with_extension("log.1");
    let _ = fs::remove_file(&old);
    fs::rename(path, &old)?;
    open_append(path)
}
