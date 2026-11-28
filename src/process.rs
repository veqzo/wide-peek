//! Process lookup with a Toolhelp snapshot.
//!
//! Anti-cheat rule: widepeek never opens a handle to the game process. It
//! only reads the system process list, like the Task Manager, and never
//! reads, writes or injects into the game.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Context;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

use crate::wide::from_wide;

/// Calls `visit` with (pid, exe name) for every process until it returns true.
fn find_in_snapshot(mut visit: impl FnMut(u32, &str) -> bool) -> Option<u32> {
    // SAFETY: plain snapshot of all processes.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = None;
    // SAFETY: `entry` has a correct size field; the snapshot is valid.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let pid = entry.th32ProcessID;
        if visit(pid, &from_wide(&entry.szExeFile)) {
            found = Some(pid);
            break;
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    // SAFETY: we own the snapshot handle.
    let _ = unsafe { CloseHandle(snapshot) };
    found
}

/// PID of the first process with this exe name (case-insensitive).
pub fn find(exe_name: &str) -> Option<u32> {
    find_in_snapshot(|_, name| name.eq_ignore_ascii_case(exe_name))
}

/// One snapshot: for each name, the PID of the first process with this exe
/// name (case-insensitive), in the order of `names`.
pub fn find_each(names: &[&str]) -> Vec<Option<u32>> {
    let mut found = vec![None; names.len()];
    find_in_snapshot(|pid, exe| {
        for (slot, name) in found.iter_mut().zip(names) {
            if slot.is_none() && exe.eq_ignore_ascii_case(name) {
                *slot = Some(pid);
            }
        }
        false
    });
    found
}

/// True if `pid` still runs with this exe name. The name check protects
/// against a reused PID.
pub fn is_running(pid: u32, exe_name: &str) -> bool {
    find_in_snapshot(|p, name| p == pid && name.eq_ignore_ascii_case(exe_name)).is_some()
}

/// Starts a program without a console and without waiting for it.
pub fn spawn_detached(exe: &Path, args: &[&str]) -> anyhow::Result<()> {
    Command::new(exe)
        .args(args)
        .creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0)
        .spawn()
        .with_context(|| format!("cannot start {}", exe.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measures one Toolhelp lookup. Run with
    /// `cargo test --release -- --ignored --nocapture snapshot_cost`.
    #[test]
    #[ignore = "measurement, not a check"]
    fn snapshot_cost() {
        let runs = 200;
        let start = std::time::Instant::now();
        for _ in 0..runs {
            std::hint::black_box(find("VALORANT.exe"));
        }
        let each = start.elapsed() / runs;
        println!("one Toolhelp lookup: {each:?}");
    }
}
