//! Monitor toggle. The device change needs administrator rights, so the
//! unelevated launcher starts the scheduled tasks `\widepeek\MonitorOff` and
//! `\widepeek\MonitorOn`. They run `widepeek monitor-off|monitor-on` elevated.

pub mod device;
pub mod tasks;

use anyhow::{Context, bail};

use crate::{config, elevation, state};
use device::DeviceStatus;

/// Entry point of the elevated `monitor-off` and `monitor-on` commands.
///
/// These commands do not write the log file: the log directory is writable by
/// the user, and an elevated process must not write there. The caller checks
/// the device state and logs the result.
pub fn command(enable: bool) -> anyhow::Result<()> {
    if !elevation::is_elevated() {
        bail!("monitor-on and monitor-off need administrator rights");
    }
    if enable {
        enable_known_or_all()
    } else {
        let config = config::load_required()?;
        if !config.toggle_monitor {
            bail!("toggle_monitor is false in the config");
        }
        set_direct(&config.monitor_instance_id, false)
    }
}

/// Enables the monitors from the config and the state. Without both, it
/// enables every disabled Monitor-class device.
fn enable_known_or_all() -> anyhow::Result<()> {
    let mut ids: Vec<String> = Vec::new();
    if let Some(config) = config::load_lenient() {
        ids.push(config.monitor_instance_id);
    }
    if let Some(id) = state::load_lenient().and_then(|s| s.monitor_instance_id)
        && !ids.iter().any(|known| known.eq_ignore_ascii_case(&id))
    {
        ids.push(id);
    }
    if ids.is_empty() {
        ids = disabled_monitors()?;
    }
    let mut first_error = None;
    for id in &ids {
        if let Err(err) = set_direct(id, true) {
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn set_direct(instance_id: &str, enable: bool) -> anyhow::Result<()> {
    match device::status(instance_id) {
        DeviceStatus::Enabled if enable => Ok(()),
        DeviceStatus::Disabled if !enable => Ok(()),
        DeviceStatus::NotFound => bail!("monitor {instance_id} is not present"),
        _ => device::set_enabled(instance_id, enable),
    }
}

/// IDs of all present Monitor-class devices that are disabled.
pub fn disabled_monitors() -> anyhow::Result<Vec<String>> {
    Ok(device::list()?
        .into_iter()
        .filter(|d| d.status == DeviceStatus::Disabled)
        .map(|d| d.instance_id)
        .collect())
}

/// Enables or disables a monitor from any process and checks the result.
///
/// Elevated: direct call. Unelevated: the scheduled task, and if that fails,
/// an elevated copy of widepeek with a UAC prompt. Returns true if the device
/// state changed.
pub fn set_enabled(instance_id: &str, enable: bool) -> anyhow::Result<bool> {
    let wanted = if enable {
        DeviceStatus::Enabled
    } else {
        DeviceStatus::Disabled
    };
    let before = device::status(instance_id);
    if before == wanted {
        return Ok(false);
    }
    if before == DeviceStatus::NotFound {
        bail!("monitor {instance_id} is not present");
    }
    let action = if enable { "enable" } else { "disable" };
    info!("{action} monitor {instance_id} (state: {before:?})");

    if elevation::is_elevated() {
        device::set_enabled(instance_id, enable)?;
    } else {
        let (task, command) = if enable {
            (tasks::MONITOR_ON, "monitor-on")
        } else {
            (tasks::MONITOR_OFF, "monitor-off")
        };
        let via_task = tasks::run(task)
            .and_then(|()| device::wait_for(instance_id, enable, device::VERIFY_TIMEOUT));
        if let Err(err) = via_task {
            warn!(
                "task {task} did not {action} the monitor: {err:#}; asking for administrator rights"
            );
            let exe = elevation::current_exe()?;
            let code = elevation::run_elevated(&exe, &[command.to_owned()])?;
            if code != 0 {
                bail!("`widepeek {command}` failed with exit code {code}");
            }
        }
    }
    device::wait_for(instance_id, enable, device::VERIFY_TIMEOUT)
        .with_context(|| format!("cannot {action} monitor {instance_id}"))?;
    info!("monitor {instance_id} is now {wanted:?}");
    Ok(true)
}

/// Enables every disabled Monitor-class device. Restore uses it when neither
/// a config nor a state exists.
pub fn enable_all_disabled() -> anyhow::Result<bool> {
    let mut changed = false;
    let mut first_error = None;
    for id in disabled_monitors()? {
        match set_enabled(&id, true) {
            Ok(c) => changed |= c,
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    first_error.map_or(Ok(changed), Err)
}
