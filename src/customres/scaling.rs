//! GPU scaling in the graphics driver, for NVIDIA and AMD.
//!
//! `DMDFO_STRETCH` needs scaling on the GPU. The AMD driver rejects the
//! stretched mode without it. Setup changes the setting once, a session sets
//! it again if the driver lost it, and uninstall restores the old value.

use std::fmt;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use super::{CONFIRM_TIMEOUT, adlx, nvapi};
use crate::console;
use crate::ui;

/// The AMD scaling settings that widepeek changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmdScaling {
    /// GPU Scaling on or off.
    pub enabled: bool,
    /// The ADLX scaling mode: 0 preserve aspect ratio, 1 full panel,
    /// 2 centered.
    pub mode: i32,
    /// Integer Scaling on or off. Full panel does not apply when it is on.
    pub integer: bool,
}

/// The largest valid ADLX scaling mode.
pub const AMD_SCALING_MODE_MAX: i32 = 2;

/// A GPU scaling setting of one vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingBefore {
    /// The NVIDIA `NV_SCALING` value.
    Nvidia(i32),
    Amd(AmdScaling),
}

impl ScalingBefore {
    pub fn nvidia(self) -> Option<i32> {
        match self {
            Self::Nvidia(value) => Some(value),
            Self::Amd(_) => None,
        }
    }

    pub fn amd(self) -> Option<AmdScaling> {
        match self {
            Self::Amd(value) => Some(value),
            Self::Nvidia(_) => None,
        }
    }

    pub fn vendor(self) -> &'static str {
        match self {
            Self::Nvidia(_) => "NVIDIA",
            Self::Amd(_) => "AMD",
        }
    }

    /// Where the user can set the value by hand.
    pub fn settings_location(self) -> &'static str {
        match self {
            Self::Nvidia(_) => {
                "the NVIDIA Control Panel > Display > Adjust desktop size and position"
            }
            Self::Amd(_) => "AMD Software > Gaming > Display > GPU Scaling and Scaling Mode",
        }
    }
}

impl fmt::Display for ScalingBefore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nvidia(value) => write!(f, "NVIDIA scaling {value}"),
            Self::Amd(s) => write!(
                f,
                "AMD GPU scaling {}, mode {}, integer scaling {}",
                on_off(s.enabled),
                s.mode,
                on_off(s.integer)
            ),
        }
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// A driver API that can read and set the GPU scaling of one display.
pub(super) trait ScalingDriver {
    fn vendor(&self) -> &'static str;
    /// The name of the setting that widepeek wants, for messages.
    fn target_name(&self) -> &'static str;
    /// The steps to set the target by hand.
    fn manual_steps(&self) -> &'static str;
    fn read(&self) -> anyhow::Result<ScalingBefore>;
    fn is_target(&self, value: ScalingBefore) -> bool;
    fn set_target(&self) -> anyhow::Result<()>;
    fn set(&self, value: ScalingBefore) -> anyhow::Result<()>;
}

/// Finds a scaling driver for the display. NVIDIA first, then AMD.
fn detect(display_name: &str) -> Option<Box<dyn ScalingDriver>> {
    nvapi::Nvidia::detect(display_name)
        .map(|n| Box::new(n) as Box<dyn ScalingDriver>)
        .or_else(|| adlx::Amd::detect(display_name).map(|a| Box::new(a) as Box<dyn ScalingDriver>))
}

/// Finds the driver of the vendor that owns `value`.
fn detect_for(display_name: &str, value: ScalingBefore) -> anyhow::Result<Box<dyn ScalingDriver>> {
    let driver = match value {
        ScalingBefore::Nvidia(_) => {
            nvapi::Nvidia::detect(display_name).map(|n| Box::new(n) as Box<dyn ScalingDriver>)
        }
        ScalingBefore::Amd(_) => {
            adlx::Amd::detect(display_name).map(|a| Box::new(a) as Box<dyn ScalingDriver>)
        }
    };
    driver.with_context(|| format!("the {} driver does not see the display", value.vendor()))
}

/// What `ensure_gpu_scaling` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuScaling {
    /// widepeek set the scaling on the GPU. `before` is the old value.
    Changed { before: ScalingBefore },
    /// The user declined the change or did not confirm it in time.
    Declined,
    /// widepeek did not change the scaling: it is already correct, no
    /// driver API is available, setup runs with `--yes`, or an error occurred.
    /// `vendor` is the driver that widepeek found, if any.
    Untouched { vendor: Option<&'static str> },
}

/// Sets the scaling of the display to full screen on the GPU. Call it with
/// the display at its native mode. Errors are not fatal: the user can set
/// the value by hand.
pub fn ensure_gpu_scaling(display_name: &str, interactive: bool) -> GpuScaling {
    let Some(driver) = detect(display_name) else {
        console::println(OTHER_SCALING_STEPS);
        return GpuScaling::Untouched { vendor: None };
    };
    match change_gpu_scaling(driver.as_ref(), interactive) {
        Ok(result) => result,
        Err(err) => {
            warn!("cannot set the {} GPU scaling: {err:#}", driver.vendor());
            console::println(&format!(
                "widepeek cannot set the GPU scaling: {err:#}\n{}",
                driver.manual_steps()
            ));
            GpuScaling::Untouched {
                vendor: Some(driver.vendor()),
            }
        }
    }
}

fn change_gpu_scaling(driver: &dyn ScalingDriver, interactive: bool) -> anyhow::Result<GpuScaling> {
    let vendor = driver.vendor();
    let target = driver.target_name();
    let before = driver.read()?;
    if driver.is_target(before) {
        console::println(&format!("The {vendor} scaling is already {target}."));
        return Ok(GpuScaling::Untouched {
            vendor: Some(vendor),
        });
    }
    if !interactive {
        console::println(driver.manual_steps());
        return Ok(GpuScaling::Untouched {
            vendor: Some(vendor),
        });
    }
    console::println(&format!(
        "\nwidepeek now sets the {vendor} scaling to {target}. The screen can go black \
         for a few seconds. If you see nothing, wait {} s: the old setting comes back.",
        CONFIRM_TIMEOUT.as_secs()
    ));
    if !console::confirm("Continue?", true)? {
        console::println(driver.manual_steps());
        return Ok(GpuScaling::Declined);
    }
    if let Err(err) = driver.set_target() {
        // A partial change must not stay behind.
        if let Err(undo) = driver.set(before) {
            warn!("cannot restore the {before}: {undo:#}");
        }
        return Err(err);
    }
    let keep = ui::ask_with_timeout(
        &format!(
            "Do you see this message clearly?\n\n\
             Click Yes to keep the new GPU scaling. Without an answer, the old setting \
             comes back in {} seconds.",
            CONFIRM_TIMEOUT.as_secs()
        ),
        CONFIRM_TIMEOUT,
    );
    if keep {
        info!("set the {vendor} scaling to {target} (was {before})");
        console::println(&format!("Set the {vendor} scaling to {target}."));
        return Ok(GpuScaling::Changed { before });
    }
    driver
        .set(before)
        .context("cannot restore the old GPU scaling")?;
    console::println(&format!("Restored the old {vendor} scaling."));
    console::println(driver.manual_steps());
    Ok(GpuScaling::Declined)
}

/// Sets the scaling on the GPU again if the driver lost it, for example
/// after a driver update. Errors are only logged: the session can go on.
pub fn keep_gpu_scaling(display_name: &str, before: ScalingBefore) {
    match reapply_gpu_scaling(display_name, before) {
        Ok(Some(target)) => info!("set the {} scaling to {target} again", before.vendor()),
        Ok(None) => {}
        Err(err) => warn!("cannot check the {} GPU scaling: {err:#}", before.vendor()),
    }
}

fn reapply_gpu_scaling(
    display_name: &str,
    before: ScalingBefore,
) -> anyhow::Result<Option<&'static str>> {
    let driver = detect_for(display_name, before)?;
    if driver.is_target(driver.read()?) {
        return Ok(None);
    }
    driver.set_target()?;
    Ok(Some(driver.target_name()))
}

/// Sets the scaling back to the value from before setup.
pub fn restore_gpu_scaling(display_name: &str, before: ScalingBefore) -> anyhow::Result<()> {
    detect_for(display_name, before)?.set(before)
}

impl ScalingDriver for nvapi::Nvidia {
    fn vendor(&self) -> &'static str {
        "NVIDIA"
    }

    fn target_name(&self) -> &'static str {
        "Full-screen on the GPU"
    }

    fn manual_steps(&self) -> &'static str {
        NVIDIA_SCALING_STEPS
    }

    fn read(&self) -> anyhow::Result<ScalingBefore> {
        self.gpu_scaling().map(ScalingBefore::Nvidia)
    }

    fn is_target(&self, value: ScalingBefore) -> bool {
        value == ScalingBefore::Nvidia(nvapi::SCALING_GPU_FULL_SCREEN)
    }

    fn set_target(&self) -> anyhow::Result<()> {
        self.set_gpu_scaling(nvapi::SCALING_GPU_FULL_SCREEN)
    }

    fn set(&self, value: ScalingBefore) -> anyhow::Result<()> {
        match value {
            ScalingBefore::Nvidia(v) => self.set_gpu_scaling(v),
            ScalingBefore::Amd(_) => bail!("{value} is not an NVIDIA setting"),
        }
    }
}

const NVIDIA_SCALING_STEPS: &str = "Set the scaling once by hand:\n  \
     NVIDIA Control Panel > Display > Adjust desktop size and position >\n  \
     Scaling mode: Full-screen, Perform scaling on: GPU. Click Apply.";

const OTHER_SCALING_STEPS: &str = "Set the GPU scaling once by hand:\n  \
     Intel: Graphics Command Center > Display > Scale: Stretch.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_by_vendor() {
        let amd = AmdScaling {
            enabled: false,
            mode: 0,
            integer: false,
        };
        assert_eq!(ScalingBefore::Nvidia(6).nvidia(), Some(6));
        assert_eq!(ScalingBefore::Nvidia(6).amd(), None);
        assert_eq!(ScalingBefore::Amd(amd).amd(), Some(amd));
        assert_eq!(ScalingBefore::Amd(amd).nvidia(), None);
    }

    #[test]
    fn describes_the_value() {
        let amd = ScalingBefore::Amd(AmdScaling {
            enabled: true,
            mode: 1,
            integer: false,
        });
        assert_eq!(
            amd.to_string(),
            "AMD GPU scaling on, mode 1, integer scaling off"
        );
        assert_eq!(ScalingBefore::Nvidia(6).to_string(), "NVIDIA scaling 6");
    }
}
