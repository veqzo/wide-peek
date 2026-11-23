//! Custom resolutions and GPU scaling in the graphics driver.
//!
//! NVIDIA (NVAPI) and AMD (ADLX) have a backend. For other drivers, setup
//! shows the manual steps and checks that Windows lists the mode.

mod adlx;
mod nvapi;
mod scaling;

pub use adlx::clean_up_pending;
pub use scaling::{
    AMD_SCALING_MODE_MAX, AmdScaling, GpuScaling, ScalingBefore, ensure_gpu_scaling,
    keep_gpu_scaling, restore_gpu_scaling,
};

use std::time::Duration;

use anyhow::bail;

use crate::console;
use crate::display::{self, DisplayMode};
use crate::ui;

/// The time the user has to confirm a trial mode before it reverts.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(15);

/// A driver API that can create custom resolutions.
pub trait CustomResBackend {
    fn name(&self) -> &'static str;
    /// True if the driver already has a custom resolution for `mode`.
    fn exists(&self, mode: DisplayMode) -> anyhow::Result<bool>;
    /// Tries `mode`, asks `confirm`, then saves it or reverts the trial.
    fn create(
        &self,
        native: DisplayMode,
        mode: DisplayMode,
        confirm: &mut dyn FnMut() -> bool,
    ) -> anyhow::Result<()>;
}

/// Finds a backend for the display, if the driver offers one.
fn detect(display_name: &str) -> Option<Box<dyn CustomResBackend>> {
    nvapi::Nvidia::detect(display_name)
        .map(|n| Box::new(n) as Box<dyn CustomResBackend>)
        .or_else(|| {
            adlx::Amd::detect(display_name).map(|a| Box::new(a) as Box<dyn CustomResBackend>)
        })
}

/// Makes sure that `display` offers `stretch`. Creates the mode with the
/// driver if possible. Fails with instructions if the mode is missing.
pub fn ensure_mode(
    display_name: &str,
    native: DisplayMode,
    stretch: DisplayMode,
    interactive: bool,
) -> anyhow::Result<()> {
    clean_up_pending();
    if display::mode_exists(display_name, stretch) {
        console::println(&format!("The driver already offers {stretch}."));
        return Ok(());
    }
    let Some(backend) = detect(display_name) else {
        console::println(&manual_instructions(stretch));
        bail!("the display does not offer {stretch} yet");
    };
    let exists = match backend.exists(stretch) {
        Ok(exists) => exists,
        Err(err) => {
            warn!(
                "cannot read the {} custom resolutions: {err:#}",
                backend.name()
            );
            console::println(&manual_instructions(stretch));
            bail!(
                "the {} driver cannot create {stretch}: {err:#}",
                backend.name()
            );
        }
    };
    if exists {
        bail!(
            "the {} driver has a custom resolution {stretch}, but Windows does not list it. \
             Reboot, then run `widepeek setup` again.",
            backend.name()
        );
    }
    if !interactive {
        console::println(&manual_instructions(stretch));
        bail!("creating a custom resolution needs the interactive setup (without --yes)");
    }

    console::println(&format!(
        "\nwidepeek now tries {stretch} with the {} driver. The screen can go black \
         for a few seconds. If you see nothing, wait {} s: the old mode comes back.",
        backend.name(),
        CONFIRM_TIMEOUT.as_secs()
    ));
    if !console::confirm("Continue?", true)? {
        bail!("custom resolution cancelled");
    }
    let mut confirm = || {
        ui::ask_with_timeout(
            &format!(
                "Do you see this message clearly at {stretch}?\n\n\
                 Click Yes to keep the resolution. Without an answer, the old mode comes \
                 back in {} seconds.",
                CONFIRM_TIMEOUT.as_secs()
            ),
            CONFIRM_TIMEOUT,
        )
    };
    backend.create(native, stretch, &mut confirm)?;
    drop(backend);

    // The trial leaves the display in the custom mode. Go back to native.
    display::apply_mode_with_retries(display_name, native, false, 5)?;
    for _ in 0..10 {
        if display::mode_exists(display_name, stretch) {
            console::println(&format!("Created the custom resolution {stretch}."));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!(
        "the driver saved {stretch}, but Windows does not list it yet. \
         Reboot, then run `widepeek setup` again."
    )
}

fn manual_instructions(stretch: DisplayMode) -> String {
    format!(
        "The display does not offer {stretch}. Create it once in the driver:\n  \
         NVIDIA: Control Panel > Display > Change resolution > Customize >\n          \
         Create Custom Resolution: {w} x {h}, {hz} Hz, Timing: Automatic.\n  \
         AMD:    Software > Gaming > Display > Custom Resolutions (accept the\n          \
         agreement there once): {w} x {h}, {hz} Hz, Timing: CVT-RB.\n  \
         Intel:  Graphics Command Center > Display > Custom.\n\
         Then run `widepeek setup` again.",
        w = stretch.width,
        h = stretch.height,
        hz = stretch.hz,
    )
}
