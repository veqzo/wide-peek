//! `widepeek setup`: the one-time setup.
//!
//! The wizard runs unelevated: display choice, the stretched
//! mode, the custom resolution, the config, the shortcuts and the optional
//! tray autostart. Only the copy of the exe to `C:\Program Files\widepeek`
//! and the scheduled tasks run elevated (see `system`).

use anyhow::{Context, bail};

use crate::cli::SetupArgs;
use crate::config::{self, Config};
use crate::display::{self, DisplayInfo, DisplayMode};
use crate::shortcut::{self, Shortcut};
use crate::{autostart, console, customres, elevation, instance, restore, riot, state, system};

/// Valorant needs at least this width, and narrower modes make no sense.
const MIN_STRETCH_WIDTH: u32 = 640;

pub fn run(args: &SetupArgs) -> anyhow::Result<()> {
    let own_console = !console::has_console();
    if own_console {
        console::alloc("widepeek setup")?;
    }
    let result = wizard(args);
    if let Err(err) = &result {
        console::eprintln(&format!(
            "
error: {err:#}"
        ));
    }
    if own_console && !args.yes {
        let _ = console::prompt(
            "
Press Enter to close this window.",
        );
    }
    result
}

fn wizard(args: &SetupArgs) -> anyhow::Result<()> {
    let _lock = instance::try_acquire(instance::SESSION)?
        .context("a stretch session is active. Close Valorant first.")?;
    if state::exists() {
        console::println("The last session did not end cleanly. Restoring the display first.");
        restore::run(false)?;
    }
    let interactive = !args.yes;
    let previous = config::load_lenient();

    console::println("widepeek setup\n");
    let screen = pick_display(args.display.as_deref(), interactive)?;
    let native = native_mode(&screen.name)?;
    let stretch = pick_stretch(args, native, interactive)?;
    // The AMD driver rejects the stretched trial while GPU Scaling is off,
    // so the scaling comes before the custom resolution.
    let scaling = customres::ensure_gpu_scaling(&screen.name, interactive);
    if let Err(err) = customres::ensure_mode(&screen.name, native, stretch, interactive) {
        if let customres::GpuScaling::Changed { before } = scaling {
            undo_gpu_scaling(&screen.name, before);
        }
        return Err(err);
    }
    let scaling_before = scaling_to_keep(previous.as_ref(), &screen.name, scaling);

    let tray_autostart = if interactive {
        console::confirm(
            "\nStart the widepeek tray at logon? It stretches the display when Valorant \
             starts. The shortcut is the more reliable way.",
            previous.as_ref().is_some_and(|c| c.tray_autostart),
        )?
    } else {
        previous.as_ref().is_some_and(|c| c.tray_autostart)
    };
    let check_updates = if interactive {
        console::confirm(
            "\nCheck GitHub for a new widepeek version once per day? widepeek asks before \
             it installs an update. GitHub sees your IP address, like for any web page.",
            previous.as_ref().is_some_and(|c| c.check_updates),
        )?
    } else {
        previous.as_ref().is_some_and(|c| c.check_updates)
    };

    let config = Config {
        display_name: screen.name.clone(),
        monitor_path: Some(screen.monitor_path.clone()).filter(|p| !p.is_empty()),
        stretch_width: stretch.width,
        stretch_height: stretch.height,
        refresh_hz: stretch.hz,
        native_width: native.width,
        native_height: native.height,
        native_hz: native.hz,
        native_bpp: native.bpp,
        toggle_monitor: previous.as_ref().is_none_or(|c| c.toggle_monitor),
        riot_client_path: previous.as_ref().and_then(|c| c.riot_client_path.clone()),
        start_timeout_secs: previous
            .as_ref()
            .map_or(config::DEFAULT_START_TIMEOUT_SECS, |c| c.start_timeout_secs),
        tray_autostart,
        check_updates,
        gpu_scaling_before: scaling_before.and_then(customres::ScalingBefore::nvidia),
        amd_scaling_before: scaling_before.and_then(customres::ScalingBefore::amd),
    };
    console::println(
        "
widepeek now installs the exe and the scheduled tasks. Confirm the Windows prompt.",
    );
    let exe = system::install(&elevation::current_user_sid()?)?;
    info!("installed {} and the tasks", exe.display());
    config::save(&config)?;
    info!("saved config for {} {native} -> {stretch}", screen.name);
    create_shortcuts(&exe)?;
    if tray_autostart {
        autostart::enable(&exe)?;
    } else {
        autostart::disable()?;
    }

    console::println(&format!(
        "\nSetup finished.\n  \
         Display:   {} {}\n  \
         Native:    {native}\n  \
         Stretched: {stretch}\n\n\
         In Valorant, set Display Mode to Fullscreen and Aspect Ratio Method to Fill once.\n\
         Start Valorant with the shortcut \"Valorant (widepeek)\".",
        screen.name, screen.monitor_name
    ));
    Ok(())
}

/// Sets the GPU scaling back after a failed setup. No config is saved then,
/// so uninstall could not restore the value later.
fn undo_gpu_scaling(display_name: &str, before: customres::ScalingBefore) {
    match customres::restore_gpu_scaling(display_name, before) {
        Ok(()) => console::println(&format!("Restored the old {} scaling.", before.vendor())),
        Err(err) => warn!("cannot restore the {before}: {err:#}"),
    }
}

/// The scaling value that uninstall restores. The value from the first
/// change stays, so uninstall restores the setting from before widepeek. A
/// value of another vendor is dropped, for example after a GPU change.
fn scaling_to_keep(
    previous: Option<&Config>,
    display_name: &str,
    scaling: customres::GpuScaling,
) -> Option<customres::ScalingBefore> {
    let managed = previous
        .filter(|c| c.display_name == display_name)
        .and_then(Config::scaling_before);
    let same_vendor =
        |vendor: Option<&str>| managed.filter(|m| vendor.is_none_or(|v| m.vendor() == v));
    match scaling {
        customres::GpuScaling::Changed { before } => {
            same_vendor(Some(before.vendor())).or(Some(before))
        }
        customres::GpuScaling::Declined => None,
        customres::GpuScaling::Untouched { vendor } => same_vendor(vendor),
    }
}

fn pick_display(wanted: Option<&str>, interactive: bool) -> anyhow::Result<DisplayInfo> {
    let mut all = display::displays();
    if all.is_empty() {
        bail!("Windows reports no active display");
    }
    if let Some(name) = wanted {
        let index = all
            .iter()
            .position(|d| d.name.eq_ignore_ascii_case(name))
            .with_context(|| format!("display {name} is not active"))?;
        return Ok(all.swap_remove(index));
    }
    let default = all.iter().position(|d| d.primary).unwrap_or(0);
    if all.len() == 1 || !interactive {
        return Ok(all.swap_remove(default));
    }
    console::println("Displays:");
    for (i, d) in all.iter().enumerate() {
        let mode =
            display::current_mode(&d.name).map_or_else(|_| "?".to_owned(), |m| m.to_string());
        let primary = if d.primary { " (primary)" } else { "" };
        console::println(&format!(
            "  {}) {}  {}  {mode}{primary}",
            i + 1,
            d.name,
            d.monitor_name
        ));
    }
    let index = choose("Display", all.len(), Some(default))?;
    Ok(all.swap_remove(index))
}

/// The current mode is the native mode. A warning shows if the display
/// offers a larger mode, because then the current mode is likely not native.
fn native_mode(display_name: &str) -> anyhow::Result<DisplayMode> {
    let native = display::current_mode(display_name)?;
    let modes = display::list_modes(display_name);
    if let Some(best) = display::best_mode(&modes)
        && (best.width, best.height) != (native.width, native.height)
    {
        warn!("current mode {native}, largest mode {best}");
        console::println(&format!(
            "Note: the display runs {native}, but it also offers {best}.\n      \
             widepeek restores {native}. Set the native mode in Windows first if this is wrong."
        ));
    }
    if let Some(hz) = higher_refresh_rate(native, &modes) {
        warn!("current mode {native}, the display also offers {hz} Hz");
        console::println(&format!(
            "Note: the display runs {} Hz, but it also offers {hz} Hz at {}x{}.\n      \
             widepeek never sets a higher refresh rate than the current one. Set {hz} Hz in \
             Windows first if you want it.",
            native.hz, native.width, native.height
        ));
    }
    console::println(&format!("\nNative mode: {native}"));
    Ok(native)
}

/// The highest refresh rate at the native resolution, if it is above the
/// current one.
fn higher_refresh_rate(native: DisplayMode, modes: &[DisplayMode]) -> Option<u32> {
    modes
        .iter()
        .filter(|m| (m.width, m.height) == (native.width, native.height))
        .map(|m| m.hz)
        .max()
        .filter(|&hz| hz > native.hz)
}

fn pick_stretch(
    args: &SetupArgs,
    native: DisplayMode,
    interactive: bool,
) -> anyhow::Result<DisplayMode> {
    let height = args.height.unwrap_or(native.height);
    let suggested = args
        .width
        .unwrap_or_else(|| display::suggest_stretch_width(height));
    let width = if interactive && args.width.is_none() {
        ask_number(
            &format!("Stretched width (height {height} x 1.45, multiple of 8)"),
            suggested,
        )?
    } else {
        suggested
    };
    let hz = args.hz.unwrap_or(native.hz);
    let stretch = DisplayMode {
        width,
        height,
        hz,
        bpp: native.bpp,
    };
    check_stretch(native, stretch)?;
    if width % 8 != 0 {
        console::println("Note: a width that is not a multiple of 8 can fail in the driver.");
    }
    console::println(&format!("Stretched mode: {stretch}"));
    Ok(stretch)
}

fn check_stretch(native: DisplayMode, stretch: DisplayMode) -> anyhow::Result<()> {
    if stretch.hz > native.hz {
        bail!(
            "{} Hz is above the native {} Hz. widepeek never sets a higher refresh rate.",
            stretch.hz,
            native.hz
        );
    }
    if stretch.height > native.height {
        bail!(
            "the height {} is above the native height {}",
            stretch.height,
            native.height
        );
    }
    if stretch.width >= native.width || stretch.width < MIN_STRETCH_WIDTH {
        bail!(
            "the stretched width must be between {MIN_STRETCH_WIDTH} and {}",
            native.width - 1
        );
    }
    Ok(())
}

/// Asks for a list index. Input and output are 1-based for the user.
fn choose(what: &str, count: usize, default: Option<usize>) -> anyhow::Result<usize> {
    loop {
        let hint = default.map_or_else(String::new, |d| format!(" [{}]", d + 1));
        let answer = console::prompt(&format!("{what}{hint}: "))?;
        if answer.is_empty()
            && let Some(d) = default
        {
            return Ok(d);
        }
        match answer.parse::<usize>() {
            Ok(n) if (1..=count).contains(&n) => return Ok(n - 1),
            _ => console::println(&format!("Enter a number from 1 to {count}.")),
        }
    }
}

fn ask_number(what: &str, default: u32) -> anyhow::Result<u32> {
    loop {
        let answer = console::prompt(&format!("{what} [{default}]: "))?;
        if answer.is_empty() {
            return Ok(default);
        }
        match answer.parse::<u32>() {
            Ok(n) if n > 0 => return Ok(n),
            _ => console::println("Enter a positive number."),
        }
    }
}

fn create_shortcuts(exe: &std::path::Path) -> anyhow::Result<()> {
    let icon = riot::valorant_exe().unwrap_or_else(|| exe.to_path_buf());
    let link = Shortcut {
        target: exe,
        args: "launch",
        icon: &icon,
        description: "Start Valorant in true stretched resolution",
    };
    for path in shortcut::locations()? {
        shortcut::create(&path, &link)?;
    }
    console::println(
        "Created the shortcut \"Valorant (widepeek)\" on the desktop and in the Start menu.",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_first_scaling_value_of_the_same_vendor() {
        use customres::{AmdScaling, GpuScaling, ScalingBefore};
        let amd = ScalingBefore::Amd(AmdScaling {
            enabled: false,
            mode: 0,
            integer: false,
        });
        let mut nvidia_config = config::sample();
        nvidia_config.gpu_scaling_before = Some(3);
        let display = nvidia_config.display_name.clone();
        let previous = Some(&nvidia_config);
        let changed = GpuScaling::Changed { before: amd };

        // After a GPU change, the old NVIDIA value is dropped.
        assert_eq!(scaling_to_keep(previous, &display, changed), Some(amd));
        let amd_untouched = GpuScaling::Untouched {
            vendor: Some("AMD"),
        };
        assert_eq!(scaling_to_keep(previous, &display, amd_untouched), None);
        // The same vendor keeps the first value.
        let nvidia_changed = GpuScaling::Changed {
            before: ScalingBefore::Nvidia(6),
        };
        assert_eq!(
            scaling_to_keep(previous, &display, nvidia_changed),
            Some(ScalingBefore::Nvidia(3))
        );
        let no_driver = GpuScaling::Untouched { vendor: None };
        assert_eq!(
            scaling_to_keep(previous, &display, no_driver),
            Some(ScalingBefore::Nvidia(3))
        );
        assert_eq!(
            scaling_to_keep(previous, &display, GpuScaling::Declined),
            None
        );
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
    fn accepts_a_normal_stretch() {
        assert!(check_stretch(mode(2560, 1440, 144), mode(2088, 1440, 144)).is_ok());
        assert!(check_stretch(mode(2560, 1440, 144), mode(1568, 1080, 120)).is_ok());
    }

    #[test]
    fn finds_a_higher_refresh_rate() {
        let modes = [
            mode(1920, 1080, 59),
            mode(1920, 1080, 144),
            mode(2560, 1440, 165),
        ];
        assert_eq!(higher_refresh_rate(mode(1920, 1080, 59), &modes), Some(144));
        assert_eq!(higher_refresh_rate(mode(1920, 1080, 144), &modes), None);
        assert_eq!(higher_refresh_rate(mode(1280, 720, 60), &modes), None);
    }

    #[test]
    fn rejects_unsafe_stretch() {
        let native = mode(2560, 1440, 144);
        assert!(check_stretch(native, mode(2088, 1440, 165)).is_err());
        assert!(check_stretch(native, mode(2560, 1440, 144)).is_err());
        assert!(check_stretch(native, mode(2088, 1600, 144)).is_err());
        assert!(check_stretch(native, mode(320, 1440, 144)).is_err());
    }
}
