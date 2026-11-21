//! Monitor toggle. The device change needs administrator rights, so the
//! unelevated launcher starts the scheduled tasks `\widepeek\MonitorOff` and
//! `\widepeek\MonitorOn`. They run `widepeek monitor-off|monitor-on` elevated.
//!
//! A session disables every monitor device, not only the one of the stretched
//! display. With one monitor still enabled, the game opens on that monitor.

pub mod device;
pub mod tasks;

use anyhow::{Context, bail};

use crate::elevation;
use device::DeviceStatus;

/// Entry point of the elevated `monitor-off` and `monitor-on` commands.
///
/// `monitor-off` disables every enabled Monitor-class device, `monitor-on`
/// enables every disabled one. The commands read no file and do not write the
/// log: the config, state and log files are writable by the user, and an
/// elevated process must not trust them. The caller checks the device states
/// and logs the result.
pub fn command(enable: bool) -> anyhow::Result<()> {
    if !elevation::is_elevated() {
        bail!("monitor-on and monitor-off need administrator rights");
    }
    let from = if enable {
        DeviceStatus::Disabled
    } else {
        DeviceStatus::Enabled
    };
    let mut first_error = None;
    for id in monitors_with(&from)? {
        if let Err(err) = device::set_enabled(&id, enable) {
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// IDs of all present Monitor-class devices with the given state.
pub fn monitors_with(status: &DeviceStatus) -> anyhow::Result<Vec<String>> {
    Ok(device::list()?
        .into_iter()
        .filter(|d| &d.status == status)
        .map(|d| d.instance_id)
        .collect())
}

/// Enables or disables monitor devices from any process and checks the result.
///
/// Elevated: direct calls. Unelevated: one run of the scheduled task, and if
/// that fails, an elevated copy of widepeek with a UAC prompt. Devices that
/// are not present are skipped with a warning. Returns true if at least one
/// device state changed.
pub fn set_all(instance_ids: &[String], enable: bool) -> anyhow::Result<bool> {
    let wanted = if enable {
        DeviceStatus::Enabled
    } else {
        DeviceStatus::Disabled
    };
    let action = if enable { "enable" } else { "disable" };
    let pending: Vec<&String> = instance_ids
        .iter()
        .filter(|id| match device::status(id) {
            DeviceStatus::NotFound => {
                warn!("monitor {id} is not present, cannot {action} it");
                false
            }
            now => now != wanted,
        })
        .collect();
    if pending.is_empty() {
        return Ok(false);
    }
    info!("{action} {} monitor device(s)", pending.len());

    if elevation::is_elevated() {
        // Try every device, so one error does not leave the others unchanged.
        let mut first_error = None;
        for id in &pending {
            if let Err(err) = device::set_enabled(id, enable) {
                first_error.get_or_insert(err);
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }
    } else {
        let (task, command) = if enable {
            (tasks::MONITOR_ON, "monitor-on")
        } else {
            (tasks::MONITOR_OFF, "monitor-off")
        };
        let via_task = tasks::run(task).and_then(|()| wait_all(&pending, enable));
        if let Err(err) = via_task {
            warn!(
                "task {task} did not {action} the monitors: {err:#}; asking for administrator rights"
            );
            let exe = elevation::current_exe()?;
            let code = elevation::run_elevated(&exe, &[command.to_owned()])?;
            if code != 0 {
                bail!("`widepeek {command}` failed with exit code {code}");
            }
        }
    }
    wait_all(&pending, enable).with_context(|| format!("cannot {action} the monitors"))?;
    info!("{} monitor device(s) are now {wanted:?}", pending.len());
    Ok(true)
}

fn wait_all(instance_ids: &[&String], enable: bool) -> anyhow::Result<()> {
    for id in instance_ids {
        device::wait_for(id, enable, device::VERIFY_TIMEOUT)?;
    }
    Ok(())
}

/// Enables every disabled Monitor-class device. Restore uses it when no
/// state tells which devices the session disabled.
pub fn enable_all_disabled() -> anyhow::Result<bool> {
    set_all(&monitors_with(&DeviceStatus::Disabled)?, true)
}
