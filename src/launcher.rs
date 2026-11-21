//! `widepeek launch`: stretch, start Valorant, wait for its end, restore.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows::core::BOOL;

use crate::process;
use crate::session::{self, StretchSession};
use crate::window::{self, HiddenWindow};
use crate::{config, console, instance, riot};

const START_POLL: Duration = Duration::from_secs(1);
const EXIT_POLL: Duration = Duration::from_secs(5);

pub fn run() -> anyhow::Result<()> {
    let _lock = instance::try_acquire(instance::LAUNCH)?.context("widepeek is already running")?;
    let config = config::load_required()?;
    if process::find(riot::GAME_PROCESS).is_some() {
        bail!("Valorant is already running. Close it, then start widepeek again.");
    }
    // Resolve the client before the display changes.
    let client = riot::client_path(config.riot_client_path.as_deref())?;
    let _window = HiddenWindow::create("widepeek-launcher", Some(launcher_proc))?;
    if console::has_console() {
        // SAFETY: the handler is a plain function without state.
        unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true) }
            .context("cannot install the console handler")?;
    }

    let session = StretchSession::begin(&config)?;
    info!("starting {}", client.display());
    process::spawn_detached(&client, &riot::LAUNCH_ARGS)?;

    let timeout = Duration::from_secs(config.start_timeout_secs);
    let pid = wait_for_start(timeout)?;
    info!("{} started (pid {pid})", riot::GAME_PROCESS);
    wait_for_exit(pid)?;
    info!("{} ended", riot::GAME_PROCESS);
    session.finish()
}

/// Polls for the game process once per second and keeps the window alive.
fn wait_for_start(timeout: Duration) -> anyhow::Result<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = process::find(riot::GAME_PROCESS) {
            return Ok(pid);
        }
        if Instant::now() >= deadline {
            bail!(
                "{} did not start within {} s",
                riot::GAME_PROCESS,
                timeout.as_secs()
            );
        }
        window::sleep(START_POLL)?;
    }
}

/// Polls the process list until the game ends. widepeek never opens a
/// handle to the game process (anti-cheat rule).
fn wait_for_exit(pid: u32) -> anyhow::Result<()> {
    while process::is_running(pid, riot::GAME_PROCESS) {
        window::sleep(EXIT_POLL)?;
    }
    Ok(())
}

extern "system" fn launcher_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    window::end_session_proc(hwnd, msg, wparam, lparam, restore_now)
}

fn restore_now() {
    let _ = session::close();
}

/// Console close, Ctrl+C, logoff and shutdown: restore, then end the process.
extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            info!("console event {ctrl_type}, restoring");
            if std::panic::catch_unwind(restore_now).is_err() {
                error!("restore in the console handler panicked");
            }
            std::process::exit(130);
        }
        _ => false.into(),
    }
}
