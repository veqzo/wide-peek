//! Restore of the native display. It is idempotent and works with a broken or
//! missing config and state file.

use std::time::Duration;

use anyhow::bail;

use crate::config::{self, Config};
use crate::display::{self, DisplayMode};
use crate::instance;
use crate::monitor::{self, device::DeviceStatus};
use crate::state::{self, SavedDisplay, State};

/// What restore sets back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub display_name: String,
    pub native: DisplayMode,
    /// The monitor devices to enable.
    pub monitor_instance_ids: Vec<String>,
    /// The other displays and their modes before the session.
    pub other_displays: Vec<SavedDisplay>,
}

impl Target {
    pub fn from_state(state: &State) -> Self {
        Self {
            display_name: state.display_name.clone(),
            native: state.native,
            monitor_instance_ids: state.monitors(),
            other_displays: state.other_displays.clone(),
        }
    }

    /// Without a state, nobody knows which devices the session disabled, so
    /// restore enables every disabled monitor device.
    pub fn from_config(config: &Config) -> Self {
        let monitor_instance_ids = if config.toggle_monitor {
            disabled_monitors()
        } else {
            Vec::new()
        };
        Self {
            display_name: config.display_name.clone(),
            native: config.native_mode(),
            monitor_instance_ids,
            other_displays: Vec::new(),
        }
    }
}

fn disabled_monitors() -> Vec<String> {
    monitor::monitors_with(&DeviceStatus::Disabled).unwrap_or_else(|err| {
        warn!("cannot list the monitor devices: {err:#}");
        Vec::new()
    })
}

/// Restores a session that did not end cleanly, but only when no session
/// runs. A running session holds the session lock and restores itself. For
/// the `RestoreAtLogon` task and the tray start, which can run at the same
/// time as a launcher that just started a new session.
pub fn run_stale(at_logon: bool) -> anyhow::Result<()> {
    let Some(_lock) = instance::try_acquire(instance::SESSION)? else {
        info!("a widepeek session is active, it restores itself");
        return Ok(());
    };
    run(at_logon)
}

/// `widepeek restore`. With `at_logon`, it does nothing when the display is
/// already normal, so the logon task costs almost nothing.
pub fn run(at_logon: bool) -> anyhow::Result<()> {
    let config = config::load_lenient();
    let state = state::load_lenient();
    if at_logon && !needs_restore(config.as_ref(), state.as_ref()) {
        info!("logon check: nothing to restore");
        return Ok(());
    }
    if let Some(state) = &state {
        info!(
            "restoring the session from the state file ({})",
            state.origin()
        );
    }
    let target = state
        .as_ref()
        .map(Target::from_state)
        .or_else(|| config.as_ref().map(Target::from_config));
    match target {
        Some(target) => restore(&target)?,
        None => restore_without_target()?,
    }
    state::clear()?;
    info!("display restored");
    Ok(())
}

fn needs_restore(config: Option<&Config>, state: Option<&State>) -> bool {
    if state.is_some() || state::exists() {
        return true;
    }
    let Some(config) = config else {
        return false;
    };
    // A session writes the state before it disables a monitor, so disabled
    // monitors without a state are not from widepeek. Do not touch them.
    display::current_mode(&config.display_name)
        .is_ok_and(|m| m.width == config.stretch_width && m.height == config.stretch_height)
}

/// Enables the monitors first, then sets the native mode and the modes of
/// the other displays. A disabled monitor limits the modes, so the order
/// matters. Collects all errors.
pub fn restore(target: &Target) -> anyhow::Result<()> {
    info!(
        "restoring {} to {} ({} monitor device(s), {} other display(s))",
        target.display_name,
        target.native,
        target.monitor_instance_ids.len(),
        target.other_displays.len()
    );
    let mut errors = Vec::new();

    match monitor::set_all(&target.monitor_instance_ids, true) {
        Ok(true) => settle(),
        Ok(false) => {}
        Err(err) => errors.push(format!("{err:#}")),
    }

    if display::current_mode(&target.display_name).ok() != Some(target.native)
        && let Err(err) = set_native(&target.display_name, target.native)
    {
        errors.push(format!("{err:#}"));
    }
    for other in &target.other_displays {
        match display::current_mode(&other.name) {
            Ok(mode) if mode == other.mode => {}
            Ok(_) => {
                if let Err(err) = set_native(&other.name, other.mode) {
                    errors.push(format!("{err:#}"));
                }
            }
            // The display was removed since the session start.
            Err(err) => warn!("skipping {}: {err:#}", other.name),
        }
    }

    finish(errors)
}

/// Sets the native mode. If that mode is gone (other driver, other cable),
/// it sets the best mode of the display, so the user never stays stretched.
fn set_native(name: &str, native: DisplayMode) -> anyhow::Result<()> {
    let err = match display::apply_mode_with_retries(name, native, false, 3) {
        Ok(()) => return Ok(()),
        Err(err) => err,
    };
    let Some(best) = display::best_mode(&display::list_modes(name)) else {
        return Err(err);
    };
    warn!("{name}: cannot set {native} ({err:#}); using {best}");
    display::apply_mode_with_retries(name, best, false, 3)
}

/// No config and no state: enable all disabled monitors and set every
/// display to its highest resolution with the highest refresh rate.
fn restore_without_target() -> anyhow::Result<()> {
    info!("no config and no state: using the best mode of each display");
    let mut errors = Vec::new();
    match monitor::enable_all_disabled() {
        Ok(true) => settle(),
        Ok(false) => {}
        Err(err) => errors.push(format!("{err:#}")),
    }
    for info in display::displays() {
        let Some(best) = display::best_mode(&display::list_modes(&info.name)) else {
            continue;
        };
        if display::current_mode(&info.name).ok() == Some(best) {
            continue;
        }
        if let Err(err) = display::apply_mode_with_retries(&info.name, best, false, 3) {
            errors.push(format!("{err:#}"));
        }
    }
    finish(errors)
}

/// The display driver needs a moment after a monitor device change.
fn settle() {
    std::thread::sleep(Duration::from_secs(1));
}

fn finish(errors: Vec<String>) -> anyhow::Result<()> {
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("restore is not complete: {}", errors.join("; "))
    }
}
