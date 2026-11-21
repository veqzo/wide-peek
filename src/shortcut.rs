//! The "Valorant (widepeek)" shortcuts on the desktop and in the Start menu.

use std::path::{Path, PathBuf};

use anyhow::Context;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface};

use crate::com::Com;
use crate::paths;

pub const FILE_NAME: &str = "Valorant (widepeek).lnk";

/// The shortcut files of the current user.
pub fn locations() -> anyhow::Result<[PathBuf; 2]> {
    Ok([
        paths::desktop()?.join(FILE_NAME),
        paths::start_menu_programs()?.join(FILE_NAME),
    ])
}

#[derive(Debug, Clone, Copy)]
pub struct Shortcut<'a> {
    pub target: &'a Path,
    pub args: &'a str,
    pub icon: &'a Path,
    pub description: &'a str,
}

/// Creates or replaces the shortcut file `path`.
pub fn create(path: &Path, link: &Shortcut<'_>) -> anyhow::Result<()> {
    let _com = Com::init()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    // SAFETY: standard shell link COM calls with valid strings.
    let result: windows::core::Result<()> = unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        shell.SetPath(&HSTRING::from(link.target))?;
        shell.SetArguments(&HSTRING::from(link.args))?;
        shell.SetDescription(&HSTRING::from(link.description))?;
        shell.SetIconLocation(&HSTRING::from(link.icon), 0)?;
        if let Some(dir) = link.target.parent() {
            shell.SetWorkingDirectory(&HSTRING::from(dir))?;
        }
        let file: IPersistFile = shell.cast()?;
        file.Save(&HSTRING::from(path), true)
    };
    result.with_context(|| format!("cannot create the shortcut {}", path.display()))?;
    info!("created shortcut {}", path.display());
    Ok(())
}

/// Deletes the shortcut files. Missing files are fine.
pub fn remove_all() -> anyhow::Result<()> {
    for path in locations()? {
        paths::remove_if_exists(&path)?;
    }
    Ok(())
}
