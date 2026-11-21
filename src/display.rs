//! Display modes: read, list, test and apply them with the GDI API.
//!
//! widepeek changes the mode with `CDS_UPDATEREGISTRY`. Valorant then sees
//! the stretched mode as the real desktop mode. Because the mode survives a
//! crash or a reboot, every path that ends a session must restore it.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows::Win32::Graphics::Gdi::{
    CDS_TEST, CDS_TYPE, CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE,
    DISP_CHANGE_BADDUALVIEW, DISP_CHANGE_BADFLAGS, DISP_CHANGE_BADMODE, DISP_CHANGE_BADPARAM,
    DISP_CHANGE_FAILED, DISP_CHANGE_NOTUPDATED, DISP_CHANGE_RESTART, DISP_CHANGE_SUCCESSFUL,
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICE_PRIMARY_DEVICE, DISPLAY_DEVICEW,
    DM_BITSPERPEL, DM_DISPLAYFIXEDOUTPUT, DM_DISPLAYFREQUENCY, DM_INTERLACED, DM_PELSHEIGHT,
    DM_PELSWIDTH, DMDFO_STRETCH, ENUM_CURRENT_SETTINGS, ENUM_DISPLAY_SETTINGS_MODE,
    EnumDisplayDevicesW, EnumDisplaySettingsW,
};
use windows::core::PCWSTR;

use crate::wide::{from_wide, to_wide};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
    pub bpp: u32,
}

impl fmt::Display for DisplayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{} @ {} Hz, {} bpp",
            self.width, self.height, self.hz, self.bpp
        )
    }
}

/// An active display (a GDI source) and the monitor connected to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    /// GDI name, for example `\\.\DISPLAY1`.
    pub name: String,
    /// Monitor name from the EDID, for example `LG ULTRAGEAR`. Can be empty.
    pub monitor_name: String,
    pub primary: bool,
    /// Device instance ID of the monitor, if Windows reports one.
    pub monitor_instance_id: Option<String>,
}

/// Suggested stretched width: `height * 1.45`, rounded to a multiple of 8.
/// 1440 gives 2088, 1080 gives 1568.
pub fn suggest_stretch_width(height: u32) -> u32 {
    let width = (u64::from(height) * 145 + 400) / 800 * 8;
    u32::try_from(width).unwrap_or(u32::MAX)
}

fn empty_devmode() -> DEVMODEW {
    DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

fn mode_of(dm: &DEVMODEW) -> DisplayMode {
    DisplayMode {
        width: dm.dmPelsWidth,
        height: dm.dmPelsHeight,
        hz: dm.dmDisplayFrequency,
        bpp: dm.dmBitsPerPel,
    }
}

fn is_interlaced(dm: &DEVMODEW) -> bool {
    // SAFETY: EnumDisplaySettingsW fills the display variant of the union.
    let flags = unsafe { dm.Anonymous2.dmDisplayFlags };
    flags & DM_INTERLACED.0 != 0
}

fn enum_settings(device: &[u16], index: ENUM_DISPLAY_SETTINGS_MODE) -> Option<DEVMODEW> {
    let mut dm = empty_devmode();
    // SAFETY: `device` is NUL-terminated and `dm` has a correct dmSize.
    let ok = unsafe { EnumDisplaySettingsW(PCWSTR(device.as_ptr()), index, &mut dm) };
    ok.as_bool().then_some(dm)
}

fn all_devmodes(device: &str) -> Vec<DEVMODEW> {
    let device = to_wide(device);
    (0..)
        .map_while(|i| enum_settings(&device, ENUM_DISPLAY_SETTINGS_MODE(i)))
        .filter(|dm| !is_interlaced(dm))
        .collect()
}

pub fn current_mode(device: &str) -> anyhow::Result<DisplayMode> {
    enum_settings(&to_wide(device), ENUM_CURRENT_SETTINGS)
        .map(|dm| mode_of(&dm))
        .with_context(|| format!("cannot read the current mode of {device}"))
}

/// All progressive modes of a display, sorted and without duplicates.
pub fn list_modes(device: &str) -> Vec<DisplayMode> {
    let mut modes: Vec<DisplayMode> = all_devmodes(device).iter().map(mode_of).collect();
    modes.sort_by_key(|m| (m.width, m.height, m.hz, m.bpp));
    modes.dedup();
    modes
}

pub fn mode_exists(device: &str, mode: DisplayMode) -> bool {
    list_modes(device).contains(&mode)
}

/// The mode with the most pixels, then the highest refresh rate, then the
/// highest color depth. Restore uses it when no config and no state exist.
pub fn best_mode(modes: &[DisplayMode]) -> Option<DisplayMode> {
    modes
        .iter()
        .copied()
        .max_by_key(|m| (u64::from(m.width) * u64::from(m.height), m.hz, m.bpp))
}

fn change(device: &[u16], dm: &DEVMODEW, flags: CDS_TYPE) -> DISP_CHANGE {
    // SAFETY: `device` is NUL-terminated and `dm` is a complete DEVMODEW.
    unsafe {
        ChangeDisplaySettingsExW(
            PCWSTR(device.as_ptr()),
            Some(dm as *const DEVMODEW),
            None,
            flags,
            None,
        )
    }
}

fn describe(code: DISP_CHANGE) -> &'static str {
    match code {
        DISP_CHANGE_RESTART => "a restart is required",
        DISP_CHANGE_FAILED => "the display driver failed",
        DISP_CHANGE_BADMODE => "the mode is not supported",
        DISP_CHANGE_NOTUPDATED => "the registry was not updated",
        DISP_CHANGE_BADFLAGS => "invalid flags",
        DISP_CHANGE_BADPARAM => "invalid parameter",
        DISP_CHANGE_BADDUALVIEW => "the system is DualView capable",
        _ => "unknown error",
    }
}

fn check(code: DISP_CHANGE, what: &str, mode: DisplayMode) -> anyhow::Result<()> {
    if code == DISP_CHANGE_SUCCESSFUL {
        Ok(())
    } else {
        bail!("{what} {mode} failed: {} ({})", describe(code), code.0)
    }
}

/// Tests the mode with `CDS_TEST`, then applies it with `CDS_UPDATEREGISTRY`.
///
/// With `stretch`, widepeek first asks Windows for stretched scaling
/// (`DMDFO_STRETCH`). If the driver rejects that, it uses the mode without it.
pub fn apply_mode(device: &str, mode: DisplayMode, stretch: bool) -> anyhow::Result<()> {
    let mut dm = all_devmodes(device)
        .into_iter()
        .find(|dm| mode_of(dm) == mode)
        .with_context(|| format!("{device} does not offer the mode {mode}"))?;
    dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_BITSPERPEL | DM_DISPLAYFREQUENCY;
    let wide = to_wide(device);

    if stretch {
        let mut scaled = dm;
        scaled.dmFields |= DM_DISPLAYFIXEDOUTPUT;
        scaled.Anonymous1.Anonymous2.dmDisplayFixedOutput = DMDFO_STRETCH;
        if change(&wide, &scaled, CDS_TEST) == DISP_CHANGE_SUCCESSFUL {
            dm = scaled;
        } else {
            warn!("{device}: stretched scaling is not accepted, using the GPU scaling setting");
        }
    }

    check(change(&wide, &dm, CDS_TEST), "testing", mode)?;
    check(change(&wide, &dm, CDS_UPDATEREGISTRY), "applying", mode)?;
    Ok(())
}

/// Applies a mode and checks the result. A display that is busy (for example
/// right after a monitor device change) gets more attempts.
pub fn apply_mode_with_retries(
    device: &str,
    mode: DisplayMode,
    stretch: bool,
    attempts: u32,
) -> anyhow::Result<()> {
    let mut last_error = None;
    for attempt in 1..=attempts.max(1) {
        match apply_mode(device, mode, stretch) {
            Ok(()) if current_mode(device).ok() == Some(mode) => {
                info!("{device}: mode is now {mode}");
                return Ok(());
            }
            Ok(()) => {
                let now = current_mode(device).map_or_else(|e| e.to_string(), |m| m.to_string());
                warn!("{device}: attempt {attempt}: mode is {now} instead of {mode}");
                last_error = Some(anyhow::anyhow!("{device} shows {now} instead of {mode}"));
            }
            Err(err) => {
                warn!("{device}: attempt {attempt}: {err:#}");
                last_error = Some(err);
            }
        }
        if attempt < attempts {
            std::thread::sleep(Duration::from_millis(750));
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("cannot set {mode} on {device}")))
}

/// All displays that are part of the desktop.
pub fn displays() -> Vec<DisplayInfo> {
    let targets = target_names();
    let mut result = Vec::new();
    for index in 0.. {
        let mut dd = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `dd` has a correct size field.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut dd, 0) }.as_bool() {
            break;
        }
        if dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != DISPLAY_DEVICE_ATTACHED_TO_DESKTOP
        {
            continue;
        }
        let name = from_wide(&dd.DeviceName);
        let (monitor_name, path) = targets
            .iter()
            .find(|(source, _, _)| source.eq_ignore_ascii_case(&name))
            .map(|(_, friendly, path)| (friendly.clone(), path.clone()))
            .unwrap_or_default();
        result.push(DisplayInfo {
            primary: dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE == DISPLAY_DEVICE_PRIMARY_DEVICE,
            monitor_instance_id: interface_path_to_instance_id(&path)
                .or_else(|| monitor_interface_of(&name)),
            monitor_name,
            name,
        });
    }
    result
}

/// Fallback for the monitor ID: the first monitor below the GDI source.
fn monitor_interface_of(source: &str) -> Option<String> {
    let source = to_wide(source);
    let mut dd = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    const EDD_GET_DEVICE_INTERFACE_NAME: u32 = 1;
    // SAFETY: `source` is NUL-terminated and `dd` has a correct size field.
    let ok = unsafe {
        EnumDisplayDevicesW(
            PCWSTR(source.as_ptr()),
            0,
            &mut dd,
            EDD_GET_DEVICE_INTERFACE_NAME,
        )
    };
    if !ok.as_bool() {
        return None;
    }
    interface_path_to_instance_id(&from_wide(&dd.DeviceID))
}

/// Maps each active GDI source name to the monitor name and the monitor
/// device interface path, using the display configuration API.
fn target_names() -> Vec<(String, String, String)> {
    let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = Vec::new();
    let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = Vec::new();
    // The configuration can change between the two calls, so retry.
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        // SAFETY: output pointers are valid.
        let err = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        };
        if err != ERROR_SUCCESS {
            return Vec::new();
        }
        paths.resize(path_count as usize, Default::default());
        modes.resize(mode_count as usize, Default::default());
        // SAFETY: buffers have the sizes that the counters report.
        let err = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if err == ERROR_SUCCESS {
            paths.truncate(path_count as usize);
            break;
        }
        if err != ERROR_INSUFFICIENT_BUFFER {
            return Vec::new();
        }
        paths.clear();
    }

    paths
        .iter()
        .filter_map(|path| {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            // SAFETY: each packet starts with a header that has the correct size.
            let ok = unsafe {
                DisplayConfigGetDeviceInfo(&mut source.header) == 0
                    && DisplayConfigGetDeviceInfo(&mut target.header) == 0
            };
            ok.then(|| {
                (
                    from_wide(&source.viewGdiDeviceName),
                    from_wide(&target.monitorFriendlyDeviceName),
                    from_wide(&target.monitorDevicePath),
                )
            })
        })
        .collect()
}

/// Converts a monitor device interface path into a device instance ID:
/// `\\?\DISPLAY#GSM5B7F#5&2a3b&0&UID4352#{e6f07b5f-...}` gives
/// `DISPLAY\GSM5B7F\5&2a3b&0&UID4352`.
pub fn interface_path_to_instance_id(path: &str) -> Option<String> {
    let rest = path.strip_prefix(r"\\?\")?;
    let (device, guid) = rest.rsplit_once('#')?;
    if !guid.starts_with('{') || device.is_empty() {
        return None;
    }
    Some(device.replace('#', "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_stretch_width() {
        assert_eq!(suggest_stretch_width(1440), 2088);
        assert_eq!(suggest_stretch_width(1080), 1568);
        assert_eq!(suggest_stretch_width(2160), 3136);
        assert_eq!(suggest_stretch_width(1440) % 8, 0);
    }

    #[test]
    fn picks_best_mode() {
        let m = |width, height, hz| DisplayMode {
            width,
            height,
            hz,
            bpp: 32,
        };
        let modes = [
            m(1920, 1080, 240),
            m(2560, 1440, 60),
            m(2560, 1440, 144),
            m(2088, 1440, 144),
        ];
        assert_eq!(best_mode(&modes), Some(m(2560, 1440, 144)));
        assert_eq!(best_mode(&[]), None);
    }

    #[test]
    fn converts_interface_path() {
        let path = r"\\?\DISPLAY#GSM5B7F#5&2a3b8f&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        assert_eq!(
            interface_path_to_instance_id(path).as_deref(),
            Some(r"DISPLAY\GSM5B7F\5&2a3b8f&0&UID4352")
        );
        assert_eq!(interface_path_to_instance_id(""), None);
        assert_eq!(interface_path_to_instance_id(r"\\?\DISPLAY#GSM5B7F"), None);
    }
}
