//! The stretch session: disable the monitor, set the stretched mode, and
//! restore everything when the session ends in any way.
//!
//! Restore paths: `finish` (normal end), `Drop` (error and panic unwind),
//! `restore_active` (tray menu) and `close` (console close, logoff, shutdown,
//! tray exit). A hard kill leaves the state file; the next start and the
//! `RestoreAtLogon` task use it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};

use crate::config::Config;
use crate::instance::{self, InstanceLock};
use crate::restore::{self, Target};
use crate::state::{self, State};
use crate::{display, monitor};

/// The target of the running session. All restore paths take it under this
/// lock, so the display is restored exactly once.
static ACTIVE: Mutex<Option<Target>> = Mutex::new(None);

/// Set by `close` under the `ACTIVE` lock. After it, no session can start, so
/// a process that ends right after `close` never leaves a stretched display.
static CLOSED: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub struct StretchSession {
    _lock: InstanceLock,
}

impl StretchSession {
    /// Starts the session. If a step fails, the guard is already active and
    /// restores what was changed.
    pub fn begin(config: &Config) -> anyhow::Result<Self> {
        let lock = instance::try_acquire(instance::SESSION)?
            .context("another widepeek session is active")?;

        if state::exists() {
            warn!("the last session did not end cleanly, restoring first");
            restore::run(false).context("cannot restore the last session")?;
        }

        let name = &config.display_name;
        let current = display::current_mode(name)
            .with_context(|| format!("display {name} from the config is not active"))?;
        let native = config.native_mode();
        let stretch = config.stretch_mode();
        if current != native {
            warn!("{name} runs {current}, the config says {native}; restore uses {native}");
        }
        let monitor_id = config
            .toggle_monitor
            .then(|| config.monitor_instance_id.clone());

        // Hold the lock during the changes. A restore from another thread
        // (tray menu, console handler, end of the Windows session) waits and
        // then undoes them, instead of running in the middle.
        let mut active = lock_active();
        if CLOSED.load(Ordering::SeqCst) {
            bail!("widepeek is closing, no new session");
        }
        let state = State::new(name.clone(), native, stretch, monitor_id);
        state::save(&state)?;
        info!("session start: {name} {native} -> {stretch}");
        *active = Some(Target::from_state(&state));
        let result = stretch_display(&state, name, stretch);
        drop(active);
        // On error, the drop of `session` restores what was changed.
        let session = Self { _lock: lock };
        result?;
        Ok(session)
    }

    /// Ends the session normally and reports restore errors.
    pub fn finish(self) -> anyhow::Result<()> {
        let result = restore_active();
        drop(self);
        match result {
            Some(result) => result,
            None => Ok(()),
        }
    }
}

fn stretch_display(state: &State, name: &str, stretch: display::DisplayMode) -> anyhow::Result<()> {
    if let Some(id) = &state.monitor_instance_id
        && monitor::set_enabled(id, false)?
    {
        // The driver rebuilds the mode list after the device change.
        std::thread::sleep(Duration::from_secs(1));
    }
    display::apply_mode_with_retries(name, stretch, true, 5)
        .context("cannot set the stretched mode")
}

impl Drop for StretchSession {
    fn drop(&mut self) {
        if let Some(Err(err)) = restore_active() {
            error!("{err:#}");
        }
    }
}

fn lock_active() -> std::sync::MutexGuard<'static, Option<Target>> {
    ACTIVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Restores the active session, if there is one. Safe to call from any
/// thread and more than once. Returns `None` if nothing was active.
pub fn restore_active() -> Option<anyhow::Result<()>> {
    let mut active = lock_active();
    let target = active.take()?;
    Some(restore_target(&target))
}

fn restore_target(target: &Target) -> anyhow::Result<()> {
    let result = restore::restore(target).and_then(|()| state::clear());
    match &result {
        Ok(()) => info!("session restored"),
        Err(err) => error!("session restore failed, the state file stays: {err:#}"),
    }
    result
}

/// Restores the active session and blocks new sessions. For the end of the
/// process: console close, logoff, shutdown and tray exit.
pub fn close() -> Option<anyhow::Result<()>> {
    let mut active = lock_active();
    CLOSED.store(true, Ordering::SeqCst);
    let target = active.take()?;
    Some(restore_target(&target))
}
