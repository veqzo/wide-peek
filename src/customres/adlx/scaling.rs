//! AMD GPU scaling: GPU Scaling, Scaling Mode and Integer Scaling.
//!
//! `DMDFO_STRETCH` works only with GPU Scaling on. Full panel stretches the
//! image, and Integer Scaling must be off, otherwise full panel does not apply.

use anyhow::bail;

use super::{AdlxError, Amd, Iface};
use crate::customres::scaling::{AmdScaling, ScalingBefore, ScalingDriver};

// IADLXDisplayServices
const DS_GET_GPU_SCALING: usize = 11;
const DS_GET_SCALING_MODE: usize = 12;
const DS_GET_INTEGER_SCALING: usize = 13;
// IADLXDisplayGPUScaling and IADLXDisplayIntegerScaling
const SWITCH_IS_ENABLED: usize = 4;
const SWITCH_SET_ENABLED: usize = 5;
// IADLXDisplayScalingMode
const MODE_GET_MODE: usize = 4;
const MODE_SET_MODE: usize = 5;

/// `ADLX_SCALE_MODE` full panel.
const FULL_PANEL: i32 = 1;

const TARGET: AmdScaling = AmdScaling {
    enabled: true,
    mode: FULL_PANEL,
    integer: false,
};

const AMD_SCALING_STEPS: &str = "Set the scaling once by hand:\n  \
     AMD Software > Gaming > Display > GPU Scaling: On, Scaling Mode: Full panel,\n  \
     Integer Scaling: Off.";

/// A value that ADLX reports as not supported. Integer Scaling and the
/// scaling mode report this while GPU Scaling is off, or when the GPU does
/// not have the feature.
fn or_default<T: Default>(result: Result<T, AdlxError>) -> Result<T, AdlxError> {
    match result {
        Err(err) if err.is_not_supported() => Ok(T::default()),
        other => other,
    }
}

impl Amd {
    fn setting(&self, index: usize, what: &'static str) -> Result<Iface, AdlxError> {
        // SAFETY: the scaling getters take a display and have an interface output.
        unsafe { self.display_services.get_for(index, &self.display, what) }
    }

    /// Reads a setting that the GPU can lack, for example Integer Scaling on
    /// older GPUs. A missing setting reads as the default.
    fn optional<T: Default>(
        &self,
        index: usize,
        what: &'static str,
        read: impl FnOnce(&Iface) -> Result<T, AdlxError>,
    ) -> Result<T, AdlxError> {
        or_default(self.setting(index, what).and_then(|setting| read(&setting)))
    }

    /// The scaling settings of the display.
    pub(super) fn scaling(&self) -> anyhow::Result<AmdScaling> {
        let gpu = self.setting(DS_GET_GPU_SCALING, "GetGPUScaling")?;
        // SAFETY: IsEnabled has a bool output.
        let enabled = unsafe { gpu.get_bool(SWITCH_IS_ENABLED, "GPUScaling IsEnabled") }?;
        let mode = self.optional(DS_GET_SCALING_MODE, "GetScalingMode", |mode| {
            // SAFETY: GetMode has an int output.
            unsafe { mode.get_i32(MODE_GET_MODE, "ScalingMode GetMode") }
        })?;
        let integer = self.optional(DS_GET_INTEGER_SCALING, "GetIntegerScaling", |integer| {
            // SAFETY: IsEnabled has a bool output.
            unsafe { integer.get_bool(SWITCH_IS_ENABLED, "IntegerScaling IsEnabled") }
        })?;
        Ok(AmdScaling {
            enabled,
            mode,
            integer,
        })
    }

    /// Sets the scaling and reads it back. The mode and Integer Scaling
    /// apply only with GPU Scaling on, so GPU Scaling goes first.
    fn set_scaling(&self, wanted: AmdScaling) -> anyhow::Result<()> {
        let now = self.scaling()?;
        if now.enabled != wanted.enabled {
            let gpu = self.setting(DS_GET_GPU_SCALING, "GetGPUScaling")?;
            // SAFETY: SetEnabled takes a bool.
            unsafe { gpu.set_bool(SWITCH_SET_ENABLED, wanted.enabled, "GPUScaling SetEnabled") }?;
        }
        if wanted.enabled {
            let now = self.scaling()?;
            // Integer Scaling blocks full panel: switch it off before the
            // mode, and on only after the mode.
            if now.integer && !wanted.integer {
                self.set_integer(false)?;
            }
            if now.mode != wanted.mode {
                let mode = self.setting(DS_GET_SCALING_MODE, "GetScalingMode")?;
                // SAFETY: SetMode takes an int.
                unsafe { mode.set_i32(MODE_SET_MODE, wanted.mode, "ScalingMode SetMode") }?;
            }
            if !now.integer && wanted.integer {
                self.set_integer(true)?;
            }
        }
        let result = self.scaling()?;
        // With GPU Scaling off, the driver ignores the other two values.
        let applied = if wanted.enabled {
            result == wanted
        } else {
            !result.enabled
        };
        if !applied {
            bail!("the AMD driver kept {}", ScalingBefore::Amd(result));
        }
        Ok(())
    }

    fn set_integer(&self, value: bool) -> anyhow::Result<()> {
        let integer = self.setting(DS_GET_INTEGER_SCALING, "GetIntegerScaling")?;
        // SAFETY: SetEnabled takes a bool.
        unsafe { integer.set_bool(SWITCH_SET_ENABLED, value, "IntegerScaling SetEnabled") }?;
        Ok(())
    }
}

impl ScalingDriver for Amd {
    fn vendor(&self) -> &'static str {
        "AMD"
    }

    fn target_name(&self) -> &'static str {
        "GPU Scaling on, Full panel"
    }

    fn manual_steps(&self) -> &'static str {
        AMD_SCALING_STEPS
    }

    fn read(&self) -> anyhow::Result<ScalingBefore> {
        self.scaling().map(ScalingBefore::Amd)
    }

    fn is_target(&self, value: ScalingBefore) -> bool {
        is_target(value)
    }

    fn set_target(&self) -> anyhow::Result<()> {
        self.set_scaling(TARGET)
    }

    fn set(&self, value: ScalingBefore) -> anyhow::Result<()> {
        match value {
            ScalingBefore::Amd(v) => self.set_scaling(v),
            ScalingBefore::Nvidia(_) => bail!("{value} is not an AMD setting"),
        }
    }
}

fn is_target(value: ScalingBefore) -> bool {
    value == ScalingBefore::Amd(TARGET)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amd(enabled: bool, mode: i32, integer: bool) -> ScalingBefore {
        ScalingBefore::Amd(AmdScaling {
            enabled,
            mode,
            integer,
        })
    }

    #[test]
    fn needs_full_panel_without_integer_scaling() {
        assert!(is_target(amd(true, FULL_PANEL, false)));
        assert!(!is_target(amd(false, FULL_PANEL, false)));
        assert!(!is_target(amd(true, 0, false)));
        assert!(!is_target(amd(true, FULL_PANEL, true)));
        assert!(!is_target(ScalingBefore::Nvidia(FULL_PANEL)));
    }

    #[test]
    fn treats_not_supported_as_off() {
        let err = AdlxError {
            what: "IntegerScaling IsEnabled",
            code: super::super::ADLX_NOT_SUPPORTED,
        };
        assert!(!or_default::<bool>(Err(err)).unwrap());
        let fail = AdlxError {
            what: "IntegerScaling IsEnabled",
            code: super::super::ADLX_FAIL,
        };
        assert!(or_default::<bool>(Err(fail)).is_err());
    }
}
