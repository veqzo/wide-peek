//! NVIDIA GPU scaling ("Adjust desktop size and position") through
//! `NvAPI_DISP_GetDisplayConfig` and `NvAPI_DISP_SetDisplayConfig`.
//!
//! The driver applies a display configuration as a whole. widepeek reads the
//! full configuration, changes only the scaling of its own display and sends
//! all paths back. Without the other paths, the driver turns other monitors
//! off.

use std::ffi::c_void;
use std::ptr;

use anyhow::{Context, bail};

use super::{NvTiming, Nvidia, version_1, zeroed};

const ID_GET_DISPLAY_CONFIG: u32 = 0x11ab_ccf8;
const ID_SET_DISPLAY_CONFIG: u32 = 0x5d8c_f8de;

/// `NV_DISPLAYCONFIG_FLAGS`
const CONFIG_VALIDATE_ONLY: u32 = 0x1;
const CONFIG_SAVE_TO_PERSISTENCE: u32 = 0x2;

/// `NV_SCALING_GPU_SCALING_TO_NATIVE`: "Full-screen", scaling on the GPU.
pub const SCALING_GPU_FULL_SCREEN: i32 = 2;

/// `MAKE_NVAPI_VERSION(type, 2)`
const fn version_2<T>() -> u32 {
    size_of::<T>() as u32 | (2 << 16)
}

/// `NV_DISPLAYCONFIG_PATH_ADVANCED_TARGET_INFO` (version 1).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AdvancedTargetInfo {
    version: u32,
    rotation: i32,
    scaling: i32,
    refresh_rate_1k: u32,
    /// Bit field: interlaced, primary, disableVirtualModeSupport, ...
    flags: u32,
    connector: i32,
    tv_format: i32,
    timing_override: i32,
    timing: NvTiming,
}

/// `NV_DISPLAYCONFIG_PATH_TARGET_INFO_V2`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct TargetInfo {
    display_id: u32,
    details: *mut AdvancedTargetInfo,
    target_id: u32,
}

/// `NV_DISPLAYCONFIG_SOURCE_MODE_INFO` (version 1).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SourceModeInfo {
    width: u32,
    height: u32,
    color_depth: u32,
    color_format: i32,
    x: i32,
    y: i32,
    spanning_orientation: i32,
    /// Bit field: bGDIPrimary, bSLIFocus, ...
    flags: u32,
}

/// `NV_DISPLAYCONFIG_PATH_INFO_V2`
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct PathInfo {
    version: u32,
    source_id: u32,
    target_info_count: u32,
    target_info: *mut TargetInfo,
    source_mode_info: *mut SourceModeInfo,
    /// Bit field: IsNonNVIDIAAdapter, reserved.
    flags: u32,
    os_adapter_id: *mut c_void,
}

type FnGetDisplayConfig = unsafe extern "C" fn(*mut u32, *mut PathInfo) -> i32;
type FnSetDisplayConfig = unsafe extern "C" fn(u32, *mut PathInfo, u32) -> i32;

/// A display configuration with all buffers that the paths point to.
/// `link` sets the pointers again right before each driver call, so the
/// buffers can change safely between the calls.
struct DisplayConfig {
    paths: Vec<PathInfo>,
    targets: Vec<Vec<TargetInfo>>,
    details: Vec<Vec<AdvancedTargetInfo>>,
    sources: Vec<SourceModeInfo>,
}

impl DisplayConfig {
    fn link(&mut self) {
        let buffers = self
            .targets
            .iter_mut()
            .zip(self.details.iter_mut())
            .zip(self.sources.iter_mut());
        for (path, ((targets, details), source)) in self.paths.iter_mut().zip(buffers) {
            for (target, detail) in targets.iter_mut().zip(details.iter_mut()) {
                target.details = detail;
            }
            path.target_info = targets.as_mut_ptr();
            path.source_mode_info = source;
        }
    }

    /// The path and target index of `display_id`.
    fn find(&self, display_id: u32) -> anyhow::Result<(usize, usize)> {
        find_target(&self.targets, display_id).with_context(|| {
            format!("the NVIDIA display configuration has no display {display_id:#010x}")
        })
    }
}

fn find_target(targets: &[Vec<TargetInfo>], display_id: u32) -> Option<(usize, usize)> {
    targets.iter().enumerate().find_map(|(p, path)| {
        path.iter()
            .position(|t| t.display_id == display_id)
            .map(|t| (p, t))
    })
}

impl Nvidia {
    /// The `NV_SCALING` value of the display.
    pub fn gpu_scaling(&self) -> anyhow::Result<i32> {
        let config = self.display_config()?;
        let (p, t) = config.find(self.display_id)?;
        Ok(config.details[p][t].scaling)
    }

    /// Sets the `NV_SCALING` value of the display and keeps all other
    /// displays as they are. The driver saves the value across reboots.
    pub fn set_gpu_scaling(&self, value: i32) -> anyhow::Result<()> {
        let set: FnSetDisplayConfig = self.function(ID_SET_DISPLAY_CONFIG)?;
        let mut config = self.display_config()?;
        let (p, t) = config.find(self.display_id)?;
        config.details[p][t].scaling = value;
        config.link();
        let count = u32::try_from(config.paths.len()).context("too many display paths")?;
        // SAFETY: `config` owns every buffer that the paths point to, and
        // `link` set the pointers after the last change.
        self.check(
            unsafe { set(count, config.paths.as_mut_ptr(), CONFIG_VALIDATE_ONLY) },
            "NvAPI_DISP_SetDisplayConfig (validate)",
        )?;
        // SAFETY: as above, nothing changed since the validation.
        self.check(
            unsafe { set(count, config.paths.as_mut_ptr(), CONFIG_SAVE_TO_PERSISTENCE) },
            "NvAPI_DISP_SetDisplayConfig",
        )?;
        let now = self.gpu_scaling()?;
        if now != value {
            bail!("the driver kept the scaling {now} instead of {value}");
        }
        Ok(())
    }

    /// Reads the configuration in three passes, as the SDK describes: the
    /// path count, then the target count of each path, then all details.
    fn display_config(&self) -> anyhow::Result<DisplayConfig> {
        const WHAT: &str = "NvAPI_DISP_GetDisplayConfig";
        let get: FnGetDisplayConfig = self.function(ID_GET_DISPLAY_CONFIG)?;

        let mut count = 0u32;
        // SAFETY: a null path array asks only for the path count.
        self.check(unsafe { get(&mut count, ptr::null_mut()) }, WHAT)?;
        let path_count = count as usize;
        let mut paths = vec![
            PathInfo {
                version: version_2::<PathInfo>(),
                ..zeroed()
            };
            path_count
        ];
        // SAFETY: `count` paths with null target and source pointers: the
        // driver fills only the target counts.
        self.check(unsafe { get(&mut count, paths.as_mut_ptr()) }, WHAT)?;
        if count as usize != path_count {
            bail!("the display configuration changed during the read, try again");
        }

        let targets = paths
            .iter()
            .map(|p| vec![zeroed::<TargetInfo>(); p.target_info_count as usize])
            .collect();
        let details = paths
            .iter()
            .map(|p| {
                vec![
                    AdvancedTargetInfo {
                        version: version_1::<AdvancedTargetInfo>(),
                        ..zeroed()
                    };
                    p.target_info_count as usize
                ]
            })
            .collect();
        let mut config = DisplayConfig {
            paths,
            targets,
            details,
            sources: vec![zeroed(); path_count],
        };
        config.link();
        // SAFETY: every path points to buffers of its own target count.
        self.check(unsafe { get(&mut count, config.paths.as_mut_ptr()) }, WHAT)?;
        let same_shape = count as usize == path_count
            && config
                .paths
                .iter()
                .zip(&config.targets)
                .all(|(p, t)| p.target_info_count as usize == t.len());
        if !same_shape {
            bail!("the display configuration changed during the read, try again");
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an NVIDIA GPU. Read-only: it never changes the display.
    /// Run with `cargo test -- --ignored scaling_reads_unelevated`.
    #[test]
    #[ignore]
    fn scaling_reads_unelevated() {
        let display = crate::display::displays()
            .into_iter()
            .find(|d| d.primary)
            .expect("no primary display");
        let nvidia = Nvidia::detect(&display.name).expect("NVAPI does not see the display");
        let scaling = nvidia.gpu_scaling().expect("cannot read the GPU scaling");
        assert!((0..=255).contains(&scaling), "scaling {scaling}");
    }

    #[test]
    fn struct_sizes_match_the_sdk() {
        assert_eq!(size_of::<AdvancedTargetInfo>(), 128);
        assert_eq!(size_of::<TargetInfo>(), 24);
        assert_eq!(size_of::<SourceModeInfo>(), 32);
        assert_eq!(size_of::<PathInfo>(), 48);
        assert_eq!(version_1::<AdvancedTargetInfo>(), 0x0001_0080);
        assert_eq!(version_2::<PathInfo>(), 0x0002_0030);
    }

    fn target(display_id: u32) -> TargetInfo {
        TargetInfo {
            display_id,
            ..zeroed()
        }
    }

    #[test]
    fn finds_the_target_in_any_path() {
        let targets = vec![vec![target(0x10)], vec![target(0x20), target(0x21)]];
        assert_eq!(find_target(&targets, 0x10), Some((0, 0)));
        assert_eq!(find_target(&targets, 0x21), Some((1, 1)));
    }

    #[test]
    fn misses_an_unknown_target() {
        let targets = vec![vec![target(0x10)], Vec::new()];
        assert_eq!(find_target(&targets, 0x30), None);
        assert_eq!(find_target(&[], 0x10), None);
    }

    #[test]
    fn link_points_to_the_own_buffers() {
        let mut config = DisplayConfig {
            paths: vec![zeroed(); 2],
            targets: vec![vec![target(1)], vec![target(2), target(3)]],
            details: vec![vec![zeroed()], vec![zeroed(); 2]],
            sources: vec![zeroed(); 2],
        };
        config.link();
        assert_eq!(config.paths[1].target_info, config.targets[1].as_mut_ptr());
        assert!(ptr::eq(
            config.paths[0].source_mode_info,
            &config.sources[0]
        ));
        assert!(ptr::eq(config.targets[1][1].details, &config.details[1][1]));
    }
}
