//! NVIDIA custom resolutions through NVAPI (`nvapi64.dll` of the driver).
//!
//! Only the documented function IDs and struct layouts of the public NVAPI
//! SDK are used. All functions come from `nvapi_QueryInterface`.

use std::ffi::{CString, c_char, c_void};

use anyhow::{Context, bail};
use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{s, w};

use super::CustomResBackend;
use crate::display::DisplayMode;

const ID_INITIALIZE: u32 = 0x0150_e828;
const ID_UNLOAD: u32 = 0xd22b_dd7e;
const ID_GET_ERROR_MESSAGE: u32 = 0x6c2d_048c;
const ID_GET_DISPLAY_ID_BY_NAME: u32 = 0xae45_7190;
const ID_GET_TIMING: u32 = 0x1751_67e9;
const ID_ENUM_CUSTOM_DISPLAY: u32 = 0xa207_2d59;
const ID_TRY_CUSTOM_DISPLAY: u32 = 0x1f7d_b630;
const ID_SAVE_CUSTOM_DISPLAY: u32 = 0x4988_2876;
const ID_REVERT_CUSTOM_DISPLAY_TRIAL: u32 = 0xcbbd_40f0;

const NVAPI_OK: i32 = 0;
const NVAPI_ERROR: i32 = -1;
const NVAPI_END_ENUMERATION: i32 = -7;

/// `NV_FORMAT`. Some drivers reject `NV_FORMAT_UNKNOWN` with the generic
/// `NVAPI_ERROR` and only accept an explicit format.
const FORMAT_UNKNOWN: i32 = 0;
const FORMAT_A8R8G8B8: i32 = 21;

/// `NV_TIMING_OVERRIDE`
const TIMING_CURRENT: i32 = 0;
const TIMING_CVT_RB: i32 = 6;

/// `MAKE_NVAPI_VERSION(type, 1)`
const fn version_1<T>() -> u32 {
    size_of::<T>() as u32 | (1 << 16)
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NvTimingExt {
    flag: u32,
    /// Refresh rate in Hz.
    rr: u16,
    /// Refresh rate in 0.001 Hz.
    rrx1k: u32,
    aspect: u32,
    rep: u16,
    status: u32,
    name: [u8; 40],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NvTiming {
    h_visible: u16,
    h_border: u16,
    h_front_porch: u16,
    h_sync_width: u16,
    h_total: u16,
    h_sync_pol: u8,
    v_visible: u16,
    v_border: u16,
    v_front_porch: u16,
    v_sync_width: u16,
    v_total: u16,
    v_sync_pol: u8,
    interlaced: u16,
    /// Pixel clock in 10 kHz.
    pclk: u32,
    etc: NvTimingExt,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NvTimingInput {
    version: u32,
    width: u32,
    height: u32,
    rr: f32,
    /// `NV_TIMING_FLAG`. All zero: progressive, no TV format.
    flag: [u32; 3],
    timing_type: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NvViewportF {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NvCustomDisplay {
    version: u32,
    width: u32,
    height: u32,
    depth: u32,
    color_format: i32,
    src_partition: NvViewportF,
    x_ratio: f32,
    y_ratio: f32,
    timing: NvTiming,
    hw_mode_set_only: u32,
}

/// Zeroed plain-old-data structs. All fields are integers or floats.
fn zeroed<T: Copy>() -> T {
    // SAFETY: only used for the repr(C) structs above, where all bits zero
    // are a valid value.
    unsafe { std::mem::zeroed() }
}

type QueryInterface = unsafe extern "C" fn(u32) -> *mut c_void;
type FnVoid = unsafe extern "C" fn() -> i32;
type FnErrorMessage = unsafe extern "C" fn(i32, *mut [c_char; 64]) -> i32;
type FnDisplayIdByName = unsafe extern "C" fn(*const c_char, *mut u32) -> i32;
type FnGetTiming = unsafe extern "C" fn(u32, *const NvTimingInput, *mut NvTiming) -> i32;
type FnEnumCustom = unsafe extern "C" fn(u32, u32, *mut NvCustomDisplay) -> i32;
type FnTryCustom = unsafe extern "C" fn(*const u32, u32, *const NvCustomDisplay) -> i32;
type FnSaveCustom = unsafe extern "C" fn(*const u32, u32, u32, u32) -> i32;
type FnRevertTrial = unsafe extern "C" fn(*const u32, u32) -> i32;

/// The loaded and initialized NVAPI for one display.
pub struct Nvidia {
    module: HMODULE,
    query: QueryInterface,
    display_id: u32,
}

impl std::fmt::Debug for Nvidia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Nvidia")
            .field("display_id", &self.display_id)
            .finish_non_exhaustive()
    }
}

impl Nvidia {
    /// Loads NVAPI and finds the display. `None` means no NVIDIA driver or
    /// the display is not on an NVIDIA GPU.
    pub fn detect(display_name: &str) -> Option<Self> {
        match Self::load(display_name) {
            Ok(nvidia) => Some(nvidia),
            Err(err) => {
                info!("no NVIDIA custom resolution support: {err:#}");
                None
            }
        }
    }

    fn load(display_name: &str) -> anyhow::Result<Self> {
        // Only System32, so a planted DLL next to the exe cannot load into
        // the elevated setup.
        // SAFETY: plain library load from the system folder.
        let module =
            unsafe { LoadLibraryExW(w!("nvapi64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
                .context("nvapi64.dll is not installed")?;
        // SAFETY: the export has this documented signature.
        let query = unsafe { GetProcAddress(module, s!("nvapi_QueryInterface")) }.map(|f| unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryInterface>(f)
        });
        let Some(query) = query else {
            // SAFETY: `module` is loaded above.
            let _ = unsafe { FreeLibrary(module) };
            bail!("nvapi64.dll has no nvapi_QueryInterface");
        };
        let mut nvidia = Self {
            module,
            query,
            display_id: 0,
        };
        let init: FnVoid = nvidia.function(ID_INITIALIZE)?;
        // SAFETY: documented call without arguments.
        nvidia.check(unsafe { init() }, "NvAPI_Initialize")?;

        let name = CString::new(display_name).context("invalid display name")?;
        let by_name: FnDisplayIdByName = nvidia.function(ID_GET_DISPLAY_ID_BY_NAME)?;
        let mut id = 0u32;
        // SAFETY: NUL-terminated name and a valid output pointer.
        nvidia.check(
            unsafe { by_name(name.as_ptr(), &mut id) },
            "NvAPI_DISP_GetDisplayIdByDisplayName",
        )?;
        nvidia.display_id = id;
        Ok(nvidia)
    }

    /// Gets a function pointer from `nvapi_QueryInterface`.
    fn function<F: Copy>(&self, id: u32) -> anyhow::Result<F> {
        // SAFETY: documented export. A null result means "not available".
        let ptr = unsafe { (self.query)(id) };
        if ptr.is_null() {
            bail!("the NVIDIA driver does not offer function {id:#010x}");
        }
        debug_assert_eq!(size_of::<F>(), size_of::<*mut c_void>());
        // SAFETY: `F` is the documented function type of `id`.
        Ok(unsafe { std::mem::transmute_copy::<*mut c_void, F>(&ptr) })
    }

    fn check(&self, status: i32, what: &str) -> anyhow::Result<()> {
        if status == NVAPI_OK {
            return Ok(());
        }
        bail!("{what} failed: {}", self.error_text(status))
    }

    fn error_text(&self, status: i32) -> String {
        let Ok(get) = self.function::<FnErrorMessage>(ID_GET_ERROR_MESSAGE) else {
            return format!("NVAPI status {status}");
        };
        let mut buf = [0 as c_char; 64];
        // SAFETY: the buffer is an NvAPI_ShortString of 64 chars.
        if unsafe { get(status, &mut buf) } != NVAPI_OK {
            return format!("NVAPI status {status}");
        }
        let bytes: Vec<u8> = buf
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        format!("{} ({status})", String::from_utf8_lossy(&bytes))
    }

    fn timing(&self, width: u32, height: u32, hz: u32, kind: i32) -> anyhow::Result<NvTiming> {
        let get: FnGetTiming = self.function(ID_GET_TIMING)?;
        let input = NvTimingInput {
            version: version_1::<NvTimingInput>(),
            width,
            height,
            rr: hz as f32,
            flag: [0; 3],
            timing_type: kind,
        };
        let mut timing: NvTiming = zeroed();
        // SAFETY: valid input and output structs with the right versions.
        let status = unsafe { get(self.display_id, &input, &mut timing) };
        self.check(status, "NvAPI_DISP_GetTiming")?;
        Ok(timing)
    }

    /// Computes the CVT reduced-blanking timing for `mode` and checks it
    /// against the timing of the native mode.
    fn safe_timing(&self, native: DisplayMode, mode: DisplayMode) -> anyhow::Result<NvTiming> {
        let native_timing = self.timing(native.width, native.height, native.hz, TIMING_CURRENT)?;
        let timing = self.timing(mode.width, mode.height, mode.hz, TIMING_CVT_RB)?;
        check_timing(&native_timing, &timing, native, mode)?;
        Ok(timing)
    }
}

/// Rejects a timing that the monitor does not already handle at the native
/// mode: higher refresh rate or higher pixel clock.
fn check_timing(
    native_timing: &NvTiming,
    timing: &NvTiming,
    native: DisplayMode,
    mode: DisplayMode,
) -> anyhow::Result<()> {
    let rr = u32::from(timing.etc.rr);
    if rr > native.hz || mode.hz > native.hz {
        bail!(
            "the computed refresh rate {rr} Hz is above the native {} Hz",
            native.hz
        );
    }
    if native_timing.pclk != 0 && timing.pclk > native_timing.pclk {
        bail!(
            "the computed pixel clock {} kHz is above the native {} kHz",
            u64::from(timing.pclk) * 10,
            u64::from(native_timing.pclk) * 10
        );
    }
    if u32::from(timing.h_visible) != mode.width || u32::from(timing.v_visible) != mode.height {
        bail!(
            "the driver computed {}x{} instead of {}x{}",
            timing.h_visible,
            timing.v_visible,
            mode.width,
            mode.height
        );
    }
    Ok(())
}

/// The color formats to try, the most widely accepted first.
fn color_formats(bpp: u32) -> &'static [i32] {
    if bpp == 32 {
        &[FORMAT_A8R8G8B8, FORMAT_UNKNOWN]
    } else {
        &[FORMAT_UNKNOWN]
    }
}

fn custom_display(mode: DisplayMode, color_format: i32, timing: NvTiming) -> NvCustomDisplay {
    NvCustomDisplay {
        version: version_1::<NvCustomDisplay>(),
        width: mode.width,
        height: mode.height,
        depth: mode.bpp,
        color_format,
        src_partition: NvViewportF {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        },
        x_ratio: 1.0,
        y_ratio: 1.0,
        timing,
        hw_mode_set_only: 0,
    }
}

impl CustomResBackend for Nvidia {
    fn name(&self) -> &'static str {
        "NVIDIA"
    }

    fn exists(&self, mode: DisplayMode) -> anyhow::Result<bool> {
        let enumerate: FnEnumCustom = self.function(ID_ENUM_CUSTOM_DISPLAY)?;
        for index in 0..64 {
            let mut custom: NvCustomDisplay = zeroed();
            custom.version = version_1::<NvCustomDisplay>();
            // SAFETY: valid output struct with the right version.
            let status = unsafe { enumerate(self.display_id, index, &mut custom) };
            if status == NVAPI_END_ENUMERATION {
                break;
            }
            self.check(status, "NvAPI_DISP_EnumCustomDisplay")?;
            if custom.width == mode.width
                && custom.height == mode.height
                && u32::from(custom.timing.etc.rr) == mode.hz
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn create(
        &self,
        native: DisplayMode,
        mode: DisplayMode,
        confirm: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<()> {
        let timing = self.safe_timing(native, mode)?;
        let ids = [self.display_id];
        let try_custom: FnTryCustom = self.function(ID_TRY_CUSTOM_DISPLAY)?;
        let save: FnSaveCustom = self.function(ID_SAVE_CUSTOM_DISPLAY)?;
        let revert: FnRevertTrial = self.function(ID_REVERT_CUSTOM_DISPLAY_TRIAL)?;
        info!(
            "trying {mode}: pclk {} kHz, htotal {}, vtotal {}",
            u64::from(timing.pclk) * 10,
            timing.h_total,
            timing.v_total
        );
        let mut status = NVAPI_ERROR;
        for &format in color_formats(mode.bpp) {
            let custom = custom_display(mode, format, timing);
            // SAFETY: one display id and one custom display of the right version.
            status = unsafe { try_custom(ids.as_ptr(), 1, &custom) };
            if status != NVAPI_ERROR {
                break;
            }
            warn!("NvAPI_DISP_TryCustomDisplay with color format {format} failed with NVAPI_ERROR");
        }
        if status == NVAPI_ERROR {
            bail!(
                "the NVIDIA driver rejected {mode} (NVAPI_ERROR). Create it once by hand: \
                 NVIDIA Control Panel > Display > Change resolution > Customize > Create \
                 Custom Resolution: {} x {}, {} Hz, Timing: CVT reduced blank. \
                 Then run `widepeek setup` again.",
                mode.width,
                mode.height,
                mode.hz
            );
        }
        self.check(status, "NvAPI_DISP_TryCustomDisplay")?;
        if !confirm() {
            // SAFETY: same display id as the trial.
            let status = unsafe { revert(ids.as_ptr(), 1) };
            self.check(status, "NvAPI_DISP_RevertCustomDisplayTrial")?;
            bail!("the custom resolution was not confirmed, the old mode is back");
        }
        // Save for this monitor only (isThisOutputIdOnly 0, isThisMonitorIdOnly 1).
        // SAFETY: same display id as the trial.
        let status = unsafe { save(ids.as_ptr(), 1, 0, 1) };
        if let Err(err) = self.check(status, "NvAPI_DISP_SaveCustomDisplay") {
            // SAFETY: same display id as the trial.
            let _ = unsafe { revert(ids.as_ptr(), 1) };
            return Err(err);
        }
        info!("saved custom resolution {mode}");
        Ok(())
    }
}

impl Drop for Nvidia {
    fn drop(&mut self) {
        if let Ok(unload) = self.function::<FnVoid>(ID_UNLOAD) {
            // SAFETY: balances NvAPI_Initialize.
            let _ = unsafe { unload() };
        }
        // SAFETY: the module was loaded in `load`.
        let _ = unsafe { FreeLibrary(self.module) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an NVIDIA GPU. Read-only: it never changes the display.
    /// Run with `cargo test -- --ignored nvapi_reads_unelevated`.
    #[test]
    #[ignore]
    fn nvapi_reads_unelevated() {
        let display = crate::display::displays()
            .into_iter()
            .find(|d| d.primary)
            .expect("no primary display");
        let nvidia = Nvidia::detect(&display.name).expect("NVAPI does not see the display");
        let native = crate::display::current_mode(&display.name).expect("no current mode");
        nvidia.exists(native).expect("cannot read the custom modes");
        nvidia
            .timing(native.width, native.height, native.hz, TIMING_CURRENT)
            .expect("cannot read the native timing");
    }

    #[test]
    fn struct_sizes_match_the_sdk() {
        assert_eq!(size_of::<NvTimingExt>(), 64);
        assert_eq!(size_of::<NvTiming>(), 96);
        assert_eq!(size_of::<NvTimingInput>(), 32);
        assert_eq!(size_of::<NvCustomDisplay>(), 144);
        assert_eq!(version_1::<NvCustomDisplay>(), 0x0001_0090);
    }

    fn timing(w: u16, h: u16, rr: u16, pclk: u32) -> NvTiming {
        let mut t: NvTiming = zeroed();
        t.h_visible = w;
        t.v_visible = h;
        t.etc.rr = rr;
        t.pclk = pclk;
        t
    }

    fn mode(width: u32, height: u32, hz: u32) -> DisplayMode {
        DisplayMode {
            width,
            height,
            hz,
            bpp: 32,
        }
    }

    #[test]
    fn accepts_lower_pixel_clock() {
        let native = timing(2560, 1440, 144, 58_600);
        let new = timing(2088, 1440, 144, 48_000);
        assert!(check_timing(&native, &new, mode(2560, 1440, 144), mode(2088, 1440, 144)).is_ok());
    }

    #[test]
    fn rejects_higher_clock_or_refresh() {
        let native = timing(2560, 1440, 144, 58_600);
        let fast = timing(2088, 1440, 144, 60_000);
        assert!(
            check_timing(&native, &fast, mode(2560, 1440, 144), mode(2088, 1440, 144)).is_err()
        );
        let high_rr = timing(2088, 1440, 165, 48_000);
        assert!(
            check_timing(
                &native,
                &high_rr,
                mode(2560, 1440, 144),
                mode(2088, 1440, 144)
            )
            .is_err()
        );
        let wrong = timing(2080, 1440, 144, 48_000);
        assert!(
            check_timing(
                &native,
                &wrong,
                mode(2560, 1440, 144),
                mode(2088, 1440, 144)
            )
            .is_err()
        );
    }
}
