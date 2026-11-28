//! The stretch session: disable all monitor devices, set the stretched mode, and
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
use crate::monitor::device::DeviceStatus;
use crate::restore::{self, Target};
use crate::state::{self, SavedDisplay, State};
use crate::{customres, display, monitor};

/// The target of the running session. All restore paths take it under this
/// lock, so the display is restored exactly once.
static ACTIVE: Mutex<Option<Target>> = Mutex::new(None);

/// How long a new session waits for the session lock. Right after a logon,
/// the `RestoreAtLogon` task can hold it for a few seconds.
const SESSION_LOCK_WAIT: Duration = Duration::from_secs(30);

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
        let lock = instance::acquire_within(instance::SESSION, SESSION_LOCK_WAIT)?
            .context("another widepeek session is active")?;

        if state::exists() {
            warn!("the last session did not end cleanly, restoring first");
            restore::run(false).context("cannot restore the last session")?;
        }

        let name = &config.display_name;
        let current = display::current_mode(name)
            .with_context(|| format!("display {name} from the config is not active"))?;
        // Before the monitor devices go off: then Windows no longer knows
        // the monitor.
        crate::config::remember_display(config);
        let native = config.native_mode();
        let stretch = config.stretch_mode();
        if current != native {
            warn!("{name} runs {current}, the config says {native}; restore uses {native}");
        }
        // All monitors go off: with one still on, the game opens there.
        let monitor_ids = if config.toggle_monitor {
            monitor::monitors_with(&DeviceStatus::Enabled)?
        } else {
            Vec::new()
        };
        let others = other_displays(name);
        if let Some(before) = config.scaling_before() {
            customres::keep_gpu_scaling(name, before);
        }

        // Hold the lock during the changes. A restore from another thread
        // (tray menu, console handler, end of the Windows session) waits and
        // then undoes them, instead of running in the middle.
        let mut active = lock_active();
        if CLOSED.load(Ordering::SeqCst) {
            bail!("widepeek is closing, no new session");
        }
        let state = State::new(name.clone(), native, stretch, monitor_ids, others);
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
    if monitor::set_all(&state.monitor_instance_ids, false)? {
        // The driver rebuilds the mode list after the device change.
        std::thread::sleep(Duration::from_secs(1));
    }
    display::apply_mode_with_retries(name, stretch, true, 5)
        .context("cannot set the stretched mode")
}

/// The modes of all active displays except `name`. Restore sets them back,
/// because a disabled monitor device can change the mode of its display.
fn other_displays(name: &str) -> Vec<SavedDisplay> {
    display::displays()
        .into_iter()
        .filter(|d| !d.name.eq_ignore_ascii_case(name))
        .filter_map(|d| match display::current_mode(&d.name) {
            Ok(mode) => Some(SavedDisplay { name: d.name, mode }),
            Err(err) => {
                warn!("cannot read the mode of {}: {err:#}", d.name);
                None
            }
        })
        .collect()
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
