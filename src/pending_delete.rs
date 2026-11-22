//! Cancels deletions that `paths::delete_at_reboot` scheduled.
//!
//! Uninstall cannot delete a running exe, so Windows deletes it at the next
//! reboot. If setup installs widepeek again before that reboot, Windows
//! would delete the new exe. Setup removes these entries first.
//!
//! Windows keeps the entries in the `REG_MULTI_SZ` value
//! `PendingFileRenameOperations`: pairs of a source and a target path. An
//! empty target means "delete the source". Only administrators can write it.

use std::path::PathBuf;

use anyhow::Context;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, REG_MULTI_SZ, RRF_RT_REG_MULTI_SZ, RegDeleteKeyValueW, RegGetValueW,
    RegSetKeyValueW,
};
use windows::core::{HSTRING, w};

const SESSION_MANAGER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager";

/// Removes the pending deletions of `paths`. Other entries stay as they are.
pub fn cancel(paths: &[PathBuf]) -> anyhow::Result<()> {
    let Some(data) = read()? else {
        return Ok(());
    };
    let wanted: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().to_lowercase())
        .collect();
    let (removed, kept) = without_deletes(&data, &wanted);
    if removed == 0 {
        return Ok(());
    }
    write(&kept)?;
    info!("cancelled {removed} deletion(s) at the next reboot");
    Ok(())
}

/// Splits the value into (source, target) pairs. The list ends at an empty
/// source or at the end of the data.
fn pairs(data: &[u16]) -> Vec<(&[u16], &[u16])> {
    let mut strings = data.split(|&c| c == 0);
    let mut pairs = Vec::new();
    while let Some(source) = strings.next() {
        if source.is_empty() {
            break;
        }
        pairs.push((source, strings.next().unwrap_or_default()));
    }
    pairs
}

/// The lowercase path of a source entry. Windows writes it as
/// `\??\C:\...`, on newer versions with a prefix such as `*1`.
fn source_path(source: &[u16]) -> String {
    let source = String::from_utf16_lossy(source).to_lowercase();
    match source.find(r"\??\") {
        Some(start) => source[start + 4..].to_owned(),
        None => source,
    }
}

/// Returns the number of removed deletions and the new value. The new value
/// is empty if no entry is left.
fn without_deletes(data: &[u16], wanted: &[String]) -> (usize, Vec<u16>) {
    let mut removed = 0;
    let mut kept = Vec::new();
    for (source, target) in pairs(data) {
        if target.is_empty() && wanted.contains(&source_path(source)) {
            removed += 1;
            continue;
        }
        kept.extend_from_slice(source);
        kept.push(0);
        kept.extend_from_slice(target);
        kept.push(0);
    }
    if !kept.is_empty() {
        kept.push(0);
    }
    (removed, kept)
}

fn read() -> anyhow::Result<Option<Vec<u16>>> {
    let key = HSTRING::from(SESSION_MANAGER);
    let value = w!("PendingFileRenameOperations");
    loop {
        let mut bytes = 0u32;
        // SAFETY: no buffer, only the size is asked for.
        let result = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                value,
                RRF_RT_REG_MULTI_SZ,
                None,
                None,
                Some(&mut bytes),
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        result
            .ok()
            .context("cannot read the pending file operations")?;
        let mut data = vec![0u16; (bytes as usize).div_ceil(2)];
        // SAFETY: `data` has room for `bytes` bytes.
        let result = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &key,
                value,
                RRF_RT_REG_MULTI_SZ,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if result == ERROR_MORE_DATA {
            // Another program added an entry between the two calls.
            continue;
        }
        result
            .ok()
            .context("cannot read the pending file operations")?;
        data.truncate(bytes as usize / 2);
        return Ok(Some(data));
    }
}

fn write(data: &[u16]) -> anyhow::Result<()> {
    let key = HSTRING::from(SESSION_MANAGER);
    let value = w!("PendingFileRenameOperations");
    if data.is_empty() {
        // SAFETY: valid key and value names.
        let result = unsafe { RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, &key, value) };
        if result != ERROR_FILE_NOT_FOUND {
            result
                .ok()
                .context("cannot clear the pending file operations")?;
        }
        return Ok(());
    }
    let bytes =
        u32::try_from(data.len() * 2).context("the pending file operations are too long")?;
    // SAFETY: `data` is a double-NUL-terminated UTF-16 list of `bytes` bytes.
    unsafe {
        RegSetKeyValueW(
            HKEY_LOCAL_MACHINE,
            &key,
            value,
            REG_MULTI_SZ.0,
            Some(data.as_ptr().cast()),
            bytes,
        )
    }
    .ok()
    .context("cannot write the pending file operations")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn wanted() -> Vec<String> {
        vec![
            r"c:\program files\widepeek\widepeek.exe".to_owned(),
            r"c:\program files\widepeek".to_owned(),
        ]
    }

    /// Read-only: parses the real value of this PC and never changes it.
    /// Run with `cargo test -- --ignored --nocapture pending_value_parses`.
    #[test]
    #[ignore]
    fn pending_value_parses() {
        let data = read().expect("cannot read the value").unwrap_or_default();
        let paths = [
            crate::paths::installed_exe().unwrap(),
            crate::paths::install_dir().unwrap(),
        ];
        let wanted: Vec<String> = paths
            .iter()
            .map(|p| p.to_string_lossy().to_lowercase())
            .collect();
        let (removed, _) = without_deletes(&data, &wanted);
        println!("{} entries, {removed} of widepeek", pairs(&data).len());
    }

    #[test]
    fn removes_only_the_own_deletions() {
        let data = utf16(concat!(
            r"*1\??\C:\Program Files\widepeek\widepeek.exe",
            "\0\0",
            r"\??\C:\other\a.tmp",
            "\0\0",
            r"\??\C:\Program Files\widepeek",
            "\0\0\0",
        ));
        let (removed, kept) = without_deletes(&data, &wanted());
        assert_eq!(removed, 2);
        assert_eq!(kept, utf16(concat!(r"\??\C:\other\a.tmp", "\0\0\0")));
    }

    #[test]
    fn keeps_renames_of_the_own_paths() {
        let data = utf16(concat!(
            r"\??\C:\Program Files\widepeek\widepeek.exe",
            "\0",
            r"!\??\C:\x.exe",
            "\0\0",
        ));
        let (removed, kept) = without_deletes(&data, &wanted());
        assert_eq!(removed, 0);
        assert_eq!(kept, data);
    }

    #[test]
    fn empties_the_value_when_nothing_is_left() {
        let data = utf16(concat!(r"\??\C:\Program Files\widepeek", "\0\0\0"));
        assert_eq!(without_deletes(&data, &wanted()), (1, Vec::new()));
    }

    #[test]
    fn reads_a_value_without_the_final_nul() {
        let data = utf16(concat!(r"\??\C:\a", "\0\0", r"\??\C:\b", "\0"));
        assert_eq!(pairs(&data).len(), 2);
        assert!(pairs(&[]).is_empty());
    }
}
