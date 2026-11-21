//! Update checks of the tray. They run on a worker thread, so the menu stays
//! responsive during a download. The worker posts `WM_TRAY_UPDATE` when it
//! ends; `wparam` is 1 if it installed an update.

use std::cell::Cell;
use std::ffi::c_void;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::update::{self, Release, Version};
use crate::{config, ui};

pub const TIMER_ID: usize = 2;
/// How often the tray looks whether the daily check is due.
pub const POLL: Duration = Duration::from_secs(60 * 60);
pub const WM_TRAY_UPDATE: u32 = WM_APP + 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// The daily check. Only if the user turned it on.
    Auto,
    /// "Check for updates" in the menu.
    Manual,
    /// "Install widepeek X.Y.Z" in the menu.
    Install,
}

thread_local! {
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// The newest release that the user did not install yet.
static PENDING: Mutex<Option<Release>> = Mutex::new(None);

pub fn is_running() -> bool {
    RUNNING.get()
}

pub fn pending_version() -> Option<Version> {
    PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|r| r.version)
}

/// Starts `job` on a worker thread, if no other update job runs.
pub fn start(hwnd: HWND, job: Job) {
    if RUNNING.get() {
        return;
    }
    if job == Job::Auto && !config::load_lenient().is_some_and(|c| c.check_updates) {
        return;
    }
    RUNNING.set(true);
    let raw = hwnd.0 as isize;
    let spawned = std::thread::Builder::new()
        .name("update".into())
        .spawn(move || worker(raw, job));
    if let Err(err) = spawned {
        error!("cannot start the update thread: {err}");
        RUNNING.set(false);
    }
}

/// Handles `WM_TRAY_UPDATE`. Returns true if the tray must restart with the
/// new exe.
pub fn finished(wparam: WPARAM) -> bool {
    RUNNING.set(false);
    wparam.0 == 1
}

fn worker(raw_hwnd: isize, job: Job) {
    let installed = match std::panic::catch_unwind(|| run(job)) {
        Ok(Ok(installed)) => installed,
        Ok(Err(err)) => {
            error!("update failed: {err:#}");
            ui::error(&format!("The update failed: {err:#}"));
            false
        }
        Err(_) => {
            error!("the update thread panicked");
            false
        }
    };
    let hwnd = HWND(raw_hwnd as *mut c_void);
    // SAFETY: PostMessageW is safe from any thread; a closed window only
    // makes the call fail.
    let _ = unsafe {
        PostMessageW(
            Some(hwnd),
            WM_TRAY_UPDATE,
            WPARAM(usize::from(installed)),
            LPARAM(0),
        )
    };
}

fn run(job: Job) -> anyhow::Result<bool> {
    let release = match job {
        Job::Auto => update::check_if_due(true),
        Job::Manual => {
            let found = update::check()?;
            if found.is_none() {
                ui::info(&format!("widepeek {} is up to date.", Version::current()));
            }
            found
        }
        Job::Install => PENDING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
    };
    let Some(release) = release else {
        return Ok(false);
    };
    set_pending(Some(release.clone()));
    let installed = update::offer(&release)?;
    if installed {
        set_pending(None);
    }
    Ok(installed)
}

fn set_pending(release: Option<Release>) {
    *PENDING.lock().unwrap_or_else(PoisonError::into_inner) = release;
}
