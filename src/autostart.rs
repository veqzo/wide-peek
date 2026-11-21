//! Tray autostart through the `Run` key of the current user. It starts the
//! tray unelevated at logon.

use std::path::Path;

use anyhow::Context;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RegDeleteKeyValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, w};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

pub fn enable(exe: &Path) -> anyhow::Result<()> {
    let command = format!("\"{}\" tray", exe.display());
    let data: Vec<u16> = command.encode_utf16().chain([0]).collect();
    let bytes = u32::try_from(data.len() * 2).context("autostart command is too long")?;
    // SAFETY: `data` is a NUL-terminated UTF-16 string of `bytes` bytes.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
            w!("widepeek"),
            REG_SZ.0,
            Some(data.as_ptr().cast()),
            bytes,
        )
    }
    .ok()
    .context("cannot write the autostart entry")?;
    info!("tray autostart enabled");
    Ok(())
}

pub fn disable() -> anyhow::Result<()> {
    // SAFETY: valid key and value names.
    let result =
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), w!("widepeek")) };
    if result != ERROR_FILE_NOT_FOUND {
        result.ok().context("cannot delete the autostart entry")?;
        info!("tray autostart disabled");
    }
    Ok(())
}
