//! Monitor-class devices: list them, read their state, enable or disable them.
//!
//! Disabling the PnP monitor device removes the EDID limits, so Windows and
//! the game accept the stretched mode in fullscreen. Only devices of the
//! Monitor setup class are ever changed.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Get_DevNode_Status, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW,
    CM_PROB, CM_PROB_DISABLED, CR_NO_SUCH_DEVNODE, CR_SUCCESS, DICS_DISABLE, DICS_ENABLE,
    DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, DIGCF_PRESENT, DN_HAS_PROBLEM, GUID_DEVCLASS_MONITOR,
    HDEVINFO, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_PROPCHANGE_PARAMS, SPDRP_DEVICEDESC,
    SPDRP_FRIENDLYNAME, SetupDiCallClassInstaller, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiSetClassInstallParamsW,
};
use windows::core::PCWSTR;

use crate::wide::{from_wide, to_wide};

/// Time to wait until a device change is visible in the device state.
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStatus {
    Enabled,
    Disabled,
    /// The device has another problem code.
    Problem(u32),
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorDevice {
    pub instance_id: String,
    pub description: String,
    pub status: DeviceStatus,
}

/// Owns a SetupAPI device information set of the Monitor class.
struct DeviceSet(HDEVINFO);

impl DeviceSet {
    fn monitors() -> anyhow::Result<Self> {
        // SAFETY: valid class GUID, no enumerator, no window.
        let set = unsafe {
            SetupDiGetClassDevsW(
                Some(&GUID_DEVCLASS_MONITOR),
                PCWSTR::null(),
                None,
                DIGCF_PRESENT,
            )
        }
        .context("cannot list monitor devices")?;
        Ok(Self(set))
    }

    fn iter(&self) -> impl Iterator<Item = SP_DEVINFO_DATA> + '_ {
        (0..).map_while(move |index| {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            // SAFETY: `data` has a correct size field.
            unsafe { SetupDiEnumDeviceInfo(self.0, index, &mut data) }
                .ok()
                .map(|()| data)
        })
    }

    fn instance_id(&self, data: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buf = [0u16; 512];
        // SAFETY: the buffer is large enough for any device instance ID.
        unsafe { SetupDiGetDeviceInstanceIdW(self.0, data, Some(&mut buf), None) }.ok()?;
        Some(from_wide(&buf))
    }

    fn description(&self, data: &SP_DEVINFO_DATA) -> String {
        [SPDRP_FRIENDLYNAME, SPDRP_DEVICEDESC]
            .into_iter()
            .find_map(|property| {
                let mut buf = [0u8; 512];
                // SAFETY: the buffer and its length match.
                unsafe {
                    SetupDiGetDeviceRegistryPropertyW(
                        self.0,
                        data,
                        property,
                        None,
                        Some(&mut buf),
                        None,
                    )
                }
                .ok()?;
                let wide: Vec<u16> = buf
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                Some(from_wide(&wide)).filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "Monitor".to_owned())
    }

    fn find(&self, instance_id: &str) -> Option<SP_DEVINFO_DATA> {
        self.iter().find(|data| {
            self.instance_id(data)
                .is_some_and(|id| id.eq_ignore_ascii_case(instance_id))
        })
    }
}

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: the set is owned by this value.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

/// All present Monitor-class devices.
pub fn list() -> anyhow::Result<Vec<MonitorDevice>> {
    let set = DeviceSet::monitors()?;
    Ok(set
        .iter()
        .filter_map(|data| {
            let instance_id = set.instance_id(&data)?;
            Some(MonitorDevice {
                status: status(&instance_id),
                description: set.description(&data),
                instance_id,
            })
        })
        .collect())
}

/// Reads the device state with the configuration manager.
pub fn status(instance_id: &str) -> DeviceStatus {
    let id = to_wide(instance_id);
    let mut devinst = 0u32;
    // SAFETY: `id` is NUL-terminated.
    let cr =
        unsafe { CM_Locate_DevNodeW(&mut devinst, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) };
    if cr == CR_NO_SUCH_DEVNODE || cr != CR_SUCCESS {
        return DeviceStatus::NotFound;
    }
    let mut flags = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    // SAFETY: output pointers are valid.
    if unsafe { CM_Get_DevNode_Status(&mut flags, &mut problem, devinst, 0) } != CR_SUCCESS {
        return DeviceStatus::NotFound;
    }
    if flags.0 & DN_HAS_PROBLEM.0 == 0 {
        DeviceStatus::Enabled
    } else if problem == CM_PROB_DISABLED {
        DeviceStatus::Disabled
    } else {
        DeviceStatus::Problem(problem.0)
    }
}

/// Enables or disables a Monitor-class device. Needs administrator rights.
pub fn set_enabled(instance_id: &str, enable: bool) -> anyhow::Result<()> {
    let set = DeviceSet::monitors()?;
    let data = set
        .find(instance_id)
        .with_context(|| format!("no monitor device with the ID {instance_id}"))?;
    let params = SP_PROPCHANGE_PARAMS {
        ClassInstallHeader: SP_CLASSINSTALL_HEADER {
            cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
            InstallFunction: DIF_PROPERTYCHANGE,
        },
        StateChange: if enable { DICS_ENABLE } else { DICS_DISABLE },
        Scope: DICS_FLAG_GLOBAL,
        HwProfile: 0,
    };
    let action = if enable { "enable" } else { "disable" };
    // SAFETY: `params` starts with its class install header and lives during the calls.
    unsafe {
        SetupDiSetClassInstallParamsW(
            set.0,
            Some(&data),
            Some(&params.ClassInstallHeader),
            size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        )
        .with_context(|| format!("cannot prepare to {action} {instance_id}"))?;
        SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.0, Some(&data))
            .with_context(|| format!("cannot {action} {instance_id}"))?;
    }
    Ok(())
}

/// Waits until the device has the expected state, for at most `timeout`.
pub fn wait_for(instance_id: &str, enabled: bool, timeout: Duration) -> anyhow::Result<()> {
    let wanted = if enabled {
        DeviceStatus::Enabled
    } else {
        DeviceStatus::Disabled
    };
    let start = Instant::now();
    loop {
        let now = status(instance_id);
        if now == wanted {
            return Ok(());
        }
        if start.elapsed() >= timeout {
            bail!(
                "monitor {instance_id} is {now:?} after {} s, expected {wanted:?}",
                timeout.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
