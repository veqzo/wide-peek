//! `widepeek launch`: stretch, start Valorant, wait for its end, restore.

use std::time::Duration;

use anyhow::{Context, bail};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows::core::BOOL;

use crate::config::{self, Config};
use crate::game::{self, Start};
use crate::process;
use crate::session::{self, StretchSession};
use crate::window::{self, HiddenWindow};
use crate::{console, instance, riot, ui, update};

pub fn run() -> anyhow::Result<()> {
    let config = launch()?;
    // The display is native again and the launch lock is free, so the
    // update can hold it. Never before the game: the user wants to play.
    if let Some(release) = update::check_if_due(config.check_updates)
        && let Err(err) = update::offer(&release)
    {
        error!("update to {} failed: {err:#}", release.version);
        ui::error(&format!("The update failed: {err:#}"));
    }
    Ok(())
}

fn launch() -> anyhow::Result<Config> {
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
    let starters = riot::start_processes(&client);
    let starters: Vec<&str> = starters.iter().map(String::as_str).collect();
    match game::wait_for_start(timeout, &starters, window::sleep)? {
        Start::Running(pid) => {
            info!("{} started (pid {pid})", riot::GAME_PROCESS);
            game::wait_for_exit(pid, window::sleep)?;
            info!("{} ended", riot::GAME_PROCESS);
        }
        Start::Cancelled => info!("the Riot Client ended before the game started"),
    }
    session.finish()?;
    Ok(config)
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
