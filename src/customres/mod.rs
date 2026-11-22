//! Custom resolutions and GPU scaling in the graphics driver.
//!
//! Only NVIDIA has a backend. For other drivers, setup shows the manual steps
//! and checks that Windows lists the mode.

mod nvapi;

use std::time::Duration;

use anyhow::{Context, bail};

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
    nvapi::Nvidia::detect(display_name).map(|n| Box::new(n) as Box<dyn CustomResBackend>)
}

/// Makes sure that `display` offers `stretch`. Creates the mode with the
/// driver if possible. Fails with instructions if the mode is missing.
pub fn ensure_mode(
    display_name: &str,
    native: DisplayMode,
    stretch: DisplayMode,
    interactive: bool,
) -> anyhow::Result<()> {
    if display::mode_exists(display_name, stretch) {
        console::println(&format!("The driver already offers {stretch}."));
        return Ok(());
    }
    let Some(backend) = detect(display_name) else {
        console::println(&manual_instructions(stretch));
        bail!("the display does not offer {stretch} yet");
    };
    if backend.exists(stretch)? {
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
         AMD:    Software > Display > Custom Resolutions.\n  \
         Intel:  Graphics Command Center > Display > Custom.\n\
         Then run `widepeek setup` again.",
        w = stretch.width,
        h = stretch.height,
        hz = stretch.hz,
    )
}

/// What `ensure_gpu_scaling` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuScaling {
    /// widepeek set Full-screen on the GPU. `before` is the old value.
    Changed { before: i32 },
    /// The user declined the change or did not confirm it in time.
    Declined,
    /// widepeek did not change the scaling: it is already correct, the
    /// driver is not NVIDIA, setup runs with `--yes`, or an error occurred.
    Untouched,
}

/// Sets the NVIDIA scaling of the display to Full-screen on the GPU. Call it
/// with the display at its native mode. Errors are not fatal, because
/// `DMDFO_STRETCH` often works without this setting.
pub fn ensure_gpu_scaling(display_name: &str, interactive: bool) -> GpuScaling {
    let Some(nvidia) = nvapi::Nvidia::detect(display_name) else {
        console::println(OTHER_SCALING_STEPS);
        return GpuScaling::Untouched;
    };
    match change_gpu_scaling(&nvidia, interactive) {
        Ok(result) => result,
        Err(err) => {
            warn!("cannot set the NVIDIA GPU scaling: {err:#}");
            console::println(&format!(
                "widepeek cannot set the GPU scaling: {err:#}\n{NVIDIA_SCALING_STEPS}"
            ));
            GpuScaling::Untouched
        }
    }
}

fn change_gpu_scaling(nvidia: &nvapi::Nvidia, interactive: bool) -> anyhow::Result<GpuScaling> {
    let before = nvidia.gpu_scaling()?;
    if before == nvapi::SCALING_GPU_FULL_SCREEN {
        console::println("The NVIDIA scaling is already Full-screen on the GPU.");
        return Ok(GpuScaling::Untouched);
    }
    if !interactive {
        console::println(NVIDIA_SCALING_STEPS);
        return Ok(GpuScaling::Untouched);
    }
    console::println(&format!(
        "\nwidepeek now sets the NVIDIA scaling to Full-screen on the GPU. The screen \
         can go black for a few seconds. If you see nothing, wait {} s: the old \
         setting comes back.",
        CONFIRM_TIMEOUT.as_secs()
    ));
    if !console::confirm("Continue?", true)? {
        console::println(NVIDIA_SCALING_STEPS);
        return Ok(GpuScaling::Declined);
    }
    nvidia.set_gpu_scaling(nvapi::SCALING_GPU_FULL_SCREEN)?;
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
        info!("set the NVIDIA scaling to Full-screen on the GPU (was {before})");
        console::println("Set the NVIDIA scaling to Full-screen on the GPU.");
        return Ok(GpuScaling::Changed { before });
    }
    nvidia
        .set_gpu_scaling(before)
        .context("cannot restore the old GPU scaling")?;
    console::println("Restored the old NVIDIA scaling.");
    console::println(NVIDIA_SCALING_STEPS);
    Ok(GpuScaling::Declined)
}

/// Sets Full-screen on the GPU again if the driver lost it, for example
/// after a driver update. Errors are only logged: the session can go on.
pub fn keep_gpu_scaling(display_name: &str) {
    match reapply_gpu_scaling(display_name) {
        Ok(true) => info!("set the NVIDIA scaling to Full-screen on the GPU again"),
        Ok(false) => {}
        Err(err) => warn!("cannot check the NVIDIA GPU scaling: {err:#}"),
    }
}

fn reapply_gpu_scaling(display_name: &str) -> anyhow::Result<bool> {
    let nvidia = nvapi::Nvidia::detect(display_name).context("NVAPI does not see the display")?;
    if nvidia.gpu_scaling()? == nvapi::SCALING_GPU_FULL_SCREEN {
        return Ok(false);
    }
    nvidia.set_gpu_scaling(nvapi::SCALING_GPU_FULL_SCREEN)?;
    Ok(true)
}

/// Sets the scaling back to the value from before setup.
pub fn restore_gpu_scaling(display_name: &str, before: i32) -> anyhow::Result<()> {
    let nvidia = nvapi::Nvidia::detect(display_name).context("NVAPI does not see the display")?;
    nvidia.set_gpu_scaling(before)
}

const NVIDIA_SCALING_STEPS: &str = "Set the scaling once by hand:\n  \
     NVIDIA Control Panel > Display > Adjust desktop size and position >\n  \
     Scaling mode: Full-screen, Perform scaling on: GPU. Click Apply.";

const OTHER_SCALING_STEPS: &str = "Set the GPU scaling once by hand:\n  \
     AMD:    Software > Display > GPU Scaling: On, Scaling Mode: Full panel.\n  \
     Intel:  Graphics Command Center > Display > Scale: Stretch.";
