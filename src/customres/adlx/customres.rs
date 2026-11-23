//! AMD custom resolutions.
//!
//! `CreateNewResolution` saves the resolution at once and has no trial. So
//! widepeek applies the new mode only temporarily, asks the user, and
//! deletes the resolution again if the user does not confirm it.
//!
//! A crash or a closed console during the trial skips that delete. So setup
//! writes a pending file before the create, and the next setup or uninstall
//! deletes a resolution that is still pending.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use super::{AdlxError, Amd, Iface, check};
use crate::customres::CustomResBackend;
use crate::display::{self, DisplayMode};
use crate::paths;

// IADLXDisplayServices
const DS_GET_CUSTOM_RESOLUTION: usize = 18;
// IADLXDisplayCustomResolution
const CR_IS_SUPPORTED: usize = 3;
const CR_GET_RESOLUTION_LIST: usize = 4;
const CR_GET_CURRENT_APPLIED_RESOLUTION: usize = 5;
const CR_CREATE_NEW_RESOLUTION: usize = 6;
const CR_DELETE_RESOLUTION: usize = 7;
// IADLXDisplayResolution
const RES_GET_VALUE: usize = 3;
const RES_SET_VALUE: usize = 4;

const PRESENTATION_PROGRESSIVE: i32 = 0;
const TIMING_CVT_RB: i32 = 1;

/// Windows lists a new resolution after about 1.3 s.
const APPEAR_TIMEOUT: Duration = Duration::from_secs(5);

const AMD_MANUAL_STEPS: &str = "Create it once by hand: AMD Software > Gaming > Display > \
     Custom Resolutions (accept the agreement there if AMD Software asks)";

/// `ADLX_TimingInfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TimingInfo {
    timing_flags: i32,
    h_total: i32,
    v_total: i32,
    h_display: i32,
    v_display: i32,
    h_front_porch: i32,
    v_front_porch: i32,
    h_sync_width: i32,
    v_sync_width: i32,
    h_polarity: i32,
    v_polarity: i32,
}

/// `ADLX_CustomResolution`. `adlx_long` is 32 bit on Windows.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CustomResolution {
    res_width: i32,
    res_height: i32,
    refresh_rate: i32,
    presentation: i32,
    timing_standard: i32,
    /// kHz.
    g_pixel_clock: i32,
    detailed_timing: TimingInfo,
}

type FnGetValue = unsafe extern "C" fn(*mut c_void, *mut CustomResolution) -> i32;
/// The C header takes the struct by value. The x64 ABI passes a struct of
/// this size as a pointer to a copy, so the pointer is the same call.
type FnSetValue = unsafe extern "C" fn(*mut c_void, *const CustomResolution) -> i32;
type FnInPtr = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;

fn value(resolution: &Iface) -> Result<CustomResolution, AdlxError> {
    let mut value = CustomResolution::default();
    // SAFETY: GetValue of IADLXDisplayResolution with a valid output.
    let code =
        unsafe { resolution.slot::<FnGetValue>(RES_GET_VALUE)(resolution.ptr(), &mut value) };
    check(code, "DisplayResolution GetValue")?;
    Ok(value)
}

/// How well `entry` matches `mode`. ADLX rounds up where Windows rounds
/// down (60 Hz for 59.94 Hz), so one more Hz is a near match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Match {
    Near,
    Exact,
}

fn match_mode(entry: &CustomResolution, mode: DisplayMode) -> Option<Match> {
    if i64::from(entry.res_width) != i64::from(mode.width)
        || i64::from(entry.res_height) != i64::from(mode.height)
    {
        return None;
    }
    match i64::from(entry.refresh_rate) - i64::from(mode.hz) {
        0 => Some(Match::Exact),
        1 => Some(Match::Near),
        _ => None,
    }
}

/// The custom resolution that setup created and the user did not confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Pending {
    display_name: String,
    mode: DisplayMode,
}

fn mark_pending(display_name: &str, mode: DisplayMode) -> anyhow::Result<()> {
    let pending = Pending {
        display_name: display_name.to_owned(),
        mode,
    };
    let json = serde_json::to_vec_pretty(&pending)?;
    paths::write_atomic(&paths::amd_pending_file()?, &json)
        .context("cannot write the AMD pending file")
}

fn clear_pending() {
    let result = paths::amd_pending_file().and_then(|path| match std::fs::remove_file(&path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(anyhow::Error::new(err).context(format!("cannot delete {}", path.display())))
        }
        _ => Ok(()),
    });
    if let Err(err) = result {
        warn!("{err:#}");
    }
}

/// Deletes the custom resolution of an earlier setup that ended before the
/// user confirmed it. Errors are only logged: the next run tries again.
pub fn clean_up_pending() {
    if let Err(err) = try_clean_up_pending() {
        warn!("cannot delete the unconfirmed AMD custom resolution: {err:#}");
    }
}

fn try_clean_up_pending() -> anyhow::Result<()> {
    let path = paths::amd_pending_file()?;
    let json = match std::fs::read(&path) {
        Ok(json) => json,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    let Ok(pending) = serde_json::from_slice::<Pending>(&json) else {
        warn!("the AMD pending file is invalid, deleting it");
        clear_pending();
        return Ok(());
    };
    let amd = Amd::detect(&pending.display_name)
        .with_context(|| format!("ADLX does not see {}", pending.display_name))?;
    let custom = amd.custom_resolution()?;
    amd.delete(&custom, pending.mode)?;
    clear_pending();
    drop(custom);
    drop(amd);
    let start = Instant::now();
    while display::mode_exists(&pending.display_name, pending.mode)
        && start.elapsed() < APPEAR_TIMEOUT
    {
        std::thread::sleep(Duration::from_millis(250));
    }
    crate::console::println(&format!(
        "Deleted the unconfirmed custom resolution {} from the last setup.",
        pending.mode
    ));
    Ok(())
}

/// The new entry, from the template of the applied native resolution. The
/// driver computes the CVT reduced-blanking timing. An all-zero timing gives
/// `ADLX_INVALID_ARGS`, so the template timing stays in the struct.
fn new_entry(template: CustomResolution, mode: DisplayMode) -> anyhow::Result<CustomResolution> {
    let width = i32::try_from(mode.width).context("width too large")?;
    let height = i32::try_from(mode.height).context("height too large")?;
    let mut entry = template;
    entry.res_width = width;
    entry.res_height = height;
    entry.refresh_rate = i32::try_from(mode.hz).context("refresh rate too large")?;
    entry.presentation = PRESENTATION_PROGRESSIVE;
    entry.timing_standard = TIMING_CVT_RB;
    entry.detailed_timing.h_display = width;
    entry.detailed_timing.v_display = height;
    Ok(entry)
}

/// Rejects an entry that the monitor does not already handle at the native
/// mode: higher refresh rate, higher pixel clock or a wrong size.
fn check_timing(
    native_entry: &CustomResolution,
    entry: &CustomResolution,
    native: DisplayMode,
    mode: DisplayMode,
) -> anyhow::Result<()> {
    let native_hz = i64::from(native.hz);
    if mode.hz > native.hz || i64::from(entry.refresh_rate) > native_hz + 1 {
        bail!(
            "the stored refresh rate {} Hz is above the native {} Hz",
            entry.refresh_rate,
            native.hz
        );
    }
    if native_entry.g_pixel_clock > 0 && entry.g_pixel_clock > native_entry.g_pixel_clock {
        bail!(
            "the computed pixel clock {} kHz is above the native {} kHz",
            entry.g_pixel_clock,
            native_entry.g_pixel_clock
        );
    }
    let timing = entry.detailed_timing;
    if i64::from(timing.h_display) != i64::from(mode.width)
        || i64::from(timing.v_display) != i64::from(mode.height)
    {
        bail!(
            "the driver computed {}x{} instead of {}x{}",
            timing.h_display,
            timing.v_display,
            mode.width,
            mode.height
        );
    }
    Ok(())
}

impl Amd {
    /// Fails with the manual steps if the display has no custom resolutions.
    fn check_supported(&self, custom: &Iface) -> anyhow::Result<()> {
        // SAFETY: IsSupported has a bool output.
        if !unsafe { custom.get_bool(CR_IS_SUPPORTED, "CustomResolution IsSupported") }? {
            bail!(
                "the AMD driver offers no custom resolutions for this display. {AMD_MANUAL_STEPS}."
            );
        }
        Ok(())
    }

    fn custom_resolution(&self) -> Result<Iface, AdlxError> {
        // SAFETY: GetCustomResolution takes a display and has an interface output.
        unsafe {
            self.display_services.get_for(
                DS_GET_CUSTOM_RESOLUTION,
                &self.display,
                "GetCustomResolution",
            )
        }
    }

    /// The resolution that the display shows now. For the hardware test.
    #[cfg(test)]
    pub(super) fn applied_resolution(&self) -> anyhow::Result<CustomResolution> {
        let custom = self.custom_resolution()?;
        // SAFETY: GetCurrentAppliedResolution has an interface output.
        let applied = unsafe {
            custom.get(
                CR_GET_CURRENT_APPLIED_RESOLUTION,
                "GetCurrentAppliedResolution",
            )
        }?;
        Ok(value(&applied)?)
    }

    /// The stored custom resolution for `mode` and its list item.
    fn find(
        &self,
        custom: &Iface,
        mode: DisplayMode,
    ) -> anyhow::Result<Option<(Iface, CustomResolution)>> {
        // SAFETY: GetResolutionList has an interface output.
        let list = unsafe { custom.get(CR_GET_RESOLUTION_LIST, "GetResolutionList") }?;
        let mut best: Option<(Match, Iface, CustomResolution)> = None;
        for item in list.items("GetResolutionList")? {
            let entry = value(&item)?;
            if let Some(quality) = match_mode(&entry, mode)
                && best.as_ref().is_none_or(|(b, _, _)| quality > *b)
            {
                best = Some((quality, item, entry));
            }
        }
        Ok(best.map(|(_, item, entry)| (item, entry)))
    }

    fn delete(&self, custom: &Iface, mode: DisplayMode) -> anyhow::Result<()> {
        let Some((item, _)) = self.find(custom, mode)? else {
            return Ok(());
        };
        // SAFETY: DeleteResolution takes an item of the resolution list.
        let code =
            unsafe { custom.slot::<FnInPtr>(CR_DELETE_RESOLUTION)(custom.ptr(), item.ptr()) };
        check(code, "DeleteResolution")?;
        info!("deleted the AMD custom resolution {mode}");
        Ok(())
    }

    /// Deletes the new resolution after a failure and keeps the first error.
    /// If the delete fails, the pending file stays for the next setup.
    fn undo(&self, custom: &Iface, mode: DisplayMode, err: anyhow::Error) -> anyhow::Error {
        match self.delete(custom, mode) {
            Ok(()) => clear_pending(),
            Err(undo) => warn!(
                "cannot delete the AMD custom resolution {mode}, the next setup tries again: {undo:#}"
            ),
        }
        err
    }

    fn wait_until_listed(&self, mode: DisplayMode) -> anyhow::Result<()> {
        let start = Instant::now();
        while start.elapsed() < APPEAR_TIMEOUT {
            if display::mode_exists(&self.display_name, mode) {
                info!(
                    "Windows lists {mode} after {} ms",
                    start.elapsed().as_millis()
                );
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        bail!(
            "Windows does not list {mode} {} s after the AMD driver created it",
            APPEAR_TIMEOUT.as_secs()
        )
    }

    /// Applies `mode` temporarily, asks the user, then goes back to the
    /// registry mode. A crash during the trial also leaves the registry
    /// mode, because the trial never writes to the registry.
    fn trial(&self, mode: DisplayMode, confirm: &mut dyn FnMut() -> bool) -> anyhow::Result<()> {
        display::apply_mode_temporary(&self.display_name, mode, true)
            .with_context(|| format!("cannot try {mode}"))?;
        let confirmed = confirm();
        if let Err(err) = display::revert_to_registry(&self.display_name) {
            // `ensure_mode` applies the native mode next in any case.
            warn!("cannot leave the trial mode: {err:#}");
        }
        if !confirmed {
            bail!("the custom resolution was not confirmed, the old mode is back");
        }
        Ok(())
    }
}

impl CustomResBackend for Amd {
    fn name(&self) -> &'static str {
        "AMD"
    }

    fn exists(&self, mode: DisplayMode) -> anyhow::Result<bool> {
        let custom = self.custom_resolution()?;
        self.check_supported(&custom)?;
        Ok(self.find(&custom, mode)?.is_some())
    }

    fn create(
        &self,
        native: DisplayMode,
        mode: DisplayMode,
        confirm: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<()> {
        let custom = self.custom_resolution()?;
        self.check_supported(&custom)?;
        // SAFETY: GetCurrentAppliedResolution has an interface output.
        let template = unsafe {
            custom.get(
                CR_GET_CURRENT_APPLIED_RESOLUTION,
                "GetCurrentAppliedResolution",
            )
        }?;
        let native_entry = value(&template)?;
        if i64::from(native_entry.res_width) != i64::from(native.width)
            || i64::from(native_entry.res_height) != i64::from(native.height)
        {
            bail!(
                "the AMD driver shows {}x{}, not the native {native}",
                native_entry.res_width,
                native_entry.res_height
            );
        }
        let entry = new_entry(native_entry, mode)?;
        // SAFETY: SetValue with a complete struct, see `FnSetValue`.
        let code = unsafe { template.slot::<FnSetValue>(RES_SET_VALUE)(template.ptr(), &entry) };
        check(code, "DisplayResolution SetValue")?;
        mark_pending(&self.display_name, mode)?;
        // SAFETY: CreateNewResolution takes a display resolution object.
        let code = unsafe {
            custom.slot::<FnInPtr>(CR_CREATE_NEW_RESOLUTION)(custom.ptr(), template.ptr())
        };
        drop(template);
        check(code, "CreateNewResolution").map_err(|err| {
            clear_pending();
            anyhow::anyhow!("the AMD driver rejected {mode}: {err}. {AMD_MANUAL_STEPS}.")
        })?;

        let stored = match self.find(&custom, mode) {
            Ok(Some((_, stored))) => stored,
            Ok(None) => {
                clear_pending();
                bail!("the AMD driver reported success, but has no custom resolution {mode}")
            }
            Err(err) => return Err(self.undo(&custom, mode, err)),
        };
        info!(
            "created {mode}: pclk {} kHz, htotal {}, vtotal {}",
            stored.g_pixel_clock, stored.detailed_timing.h_total, stored.detailed_timing.v_total
        );
        check_timing(&native_entry, &stored, native, mode)
            .and_then(|()| self.wait_until_listed(mode))
            .and_then(|()| self.trial(mode, confirm))
            .map_err(|err| self.undo(&custom, mode, err))?;
        clear_pending();
        info!("saved custom resolution {mode}");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, hz: u32) -> DisplayMode {
        DisplayMode {
            width,
            height,
            hz,
            bpp: 32,
        }
    }

    fn entry(width: i32, height: i32, hz: i32, pclk: i32) -> CustomResolution {
        CustomResolution {
            res_width: width,
            res_height: height,
            refresh_rate: hz,
            g_pixel_clock: pclk,
            detailed_timing: TimingInfo {
                h_display: width,
                v_display: height,
                ..TimingInfo::default()
            },
            ..CustomResolution::default()
        }
    }

    #[test]
    fn struct_sizes_match_the_sdk() {
        assert_eq!(size_of::<TimingInfo>(), 44);
        assert_eq!(size_of::<CustomResolution>(), 68);
    }

    #[test]
    fn builds_the_entry_from_the_template() {
        let template = CustomResolution {
            timing_standard: 4,
            detailed_timing: TimingInfo {
                h_total: 2200,
                v_total: 1125,
                ..TimingInfo::default()
            },
            ..entry(1920, 1080, 60, 148_350)
        };
        let new = new_entry(template, mode(1440, 1080, 144)).unwrap();
        assert_eq!(
            (new.res_width, new.res_height, new.refresh_rate),
            (1440, 1080, 144)
        );
        assert_eq!(new.timing_standard, TIMING_CVT_RB);
        assert_eq!(new.presentation, PRESENTATION_PROGRESSIVE);
        assert_eq!(new.detailed_timing.h_display, 1440);
        assert_eq!(new.detailed_timing.h_total, 2200);
    }

    #[test]
    fn prefers_the_exact_refresh_rate() {
        assert_eq!(
            match_mode(&entry(1440, 1080, 144, 0), mode(1440, 1080, 144)),
            Some(Match::Exact)
        );
        // ADLX reports 60 Hz where Windows reports 59 Hz.
        assert_eq!(
            match_mode(&entry(1440, 1080, 60, 0), mode(1440, 1080, 59)),
            Some(Match::Near)
        );
        // A user entry with 1 Hz less is a different resolution.
        assert_eq!(
            match_mode(&entry(1440, 1080, 143, 0), mode(1440, 1080, 144)),
            None
        );
        assert_eq!(
            match_mode(&entry(1440, 1080, 144, 0), mode(1440, 1080, 120)),
            None
        );
        assert_eq!(
            match_mode(&entry(1440, 1080, 60, 0), mode(1280, 1080, 60)),
            None
        );
        assert!(Match::Exact > Match::Near);
    }

    #[test]
    fn pending_file_round_trips() {
        let pending = Pending {
            display_name: r"\\.\DISPLAY1".to_owned(),
            mode: mode(1440, 1080, 144),
        };
        let json = serde_json::to_vec(&pending).unwrap();
        assert_eq!(serde_json::from_slice::<Pending>(&json).unwrap(), pending);
    }

    #[test]
    fn accepts_a_lower_pixel_clock() {
        // Values from the RX 9070 XT test.
        let native = entry(1920, 1080, 144, 325_080);
        let new = entry(1440, 1080, 144, 266_500);
        assert!(check_timing(&native, &new, mode(1920, 1080, 144), mode(1440, 1080, 144)).is_ok());
        let native_59 = entry(1920, 1080, 60, 148_350);
        let new_59 = entry(1440, 1080, 60, 104_000);
        assert!(
            check_timing(
                &native_59,
                &new_59,
                mode(1920, 1080, 59),
                mode(1440, 1080, 59)
            )
            .is_ok()
        );
    }

    #[test]
    fn rejects_higher_clock_refresh_or_wrong_size() {
        let native = entry(1920, 1080, 60, 148_350);
        let fast = entry(1440, 1080, 144, 266_500);
        assert!(check_timing(&native, &fast, mode(1920, 1080, 59), mode(1440, 1080, 59)).is_err());
        assert!(check_timing(&native, &fast, mode(1920, 1080, 60), mode(1440, 1080, 144)).is_err());
        let mut wrong = entry(1440, 1080, 60, 100_000);
        wrong.detailed_timing.h_display = 1432;
        assert!(check_timing(&native, &wrong, mode(1920, 1080, 60), mode(1440, 1080, 60)).is_err());
    }
}
