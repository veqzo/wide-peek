//! `widepeek uninstall`: restore the display, then remove everything that
//! setup created.

use std::fs;
use std::path::Path;

use anyhow::Context;

use crate::{
    autostart, config, console, customres, elevation, instance, log, paths, restore, shortcut,
    system, ui,
};

pub fn run(yes: bool) -> anyhow::Result<()> {
    if !yes && !confirm()? {
        info!("uninstall cancelled");
        return Ok(());
    }
    let lock = instance::try_acquire(instance::SESSION)?
        .context("a stretch session is active. Close Valorant first.")?;
    // Restore while the monitor tasks still exist.
    restore::run(false).context("cannot restore the display, nothing was removed")?;
    restore_gpu_scaling();
    if !elevation::is_elevated() {
        console::println("Uninstall needs administrator rights. Confirm the Windows prompt.");
    }
    // The elevated step removes only the tasks and the program folder. The
    // files of the user are removed here, without administrator rights.
    system::uninstall()?;
    autostart::disable()?;
    shortcut::remove_all()?;
    remove_dir(&paths::config_dir()?)?;
    info!("uninstall finished");
    drop(lock);
    // The log goes last. Nothing logs after this point.
    log::close();
    remove_dir(&paths::data_dir()?)?;
    show("widepeek is uninstalled.");
    Ok(())
}

/// Sets the NVIDIA scaling back to the value from before setup. A failure
/// does not stop the uninstall: the user can set the value by hand.
fn restore_gpu_scaling() {
    let Some(config) = config::load_lenient() else {
        return;
    };
    let Some(before) = config.gpu_scaling_before else {
        return;
    };
    match customres::restore_gpu_scaling(&config.display_name, before) {
        Ok(()) => info!("restored the NVIDIA scaling {before}"),
        Err(err) => {
            warn!("cannot restore the NVIDIA scaling {before}: {err:#}");
            show(
                "widepeek cannot restore the old GPU scaling. Set it in the NVIDIA \
                 Control Panel > Display > Adjust desktop size and position.",
            );
        }
    }
}

fn confirm() -> anyhow::Result<bool> {
    let question = "Uninstall widepeek? This restores the display and removes the tasks, \
                    the shortcuts, the program files and the configuration.";
    if console::has_console() {
        console::confirm(question, false)
    } else {
        Ok(ui::ask(question))
    }
}

fn show(text: &str) {
    if console::has_console() {
        console::println(text);
    } else {
        ui::info(text);
    }
}

fn remove_dir(dir: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot delete {}", dir.display())),
    }
}
