//! Process lookup with a Toolhelp snapshot and a wait handle for the game.
//!
//! Anti-cheat rule: widepeek opens the game only with `SYNCHRONIZE`. It never
//! asks for other access rights and never reads, writes or injects into it.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::Context;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS, OpenProcess, PROCESS_SYNCHRONIZE,
};

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

/// True if `pid` still runs with this exe name. The name check protects
/// against a reused PID.
pub fn is_running(pid: u32, exe_name: &str) -> bool {
    find_in_snapshot(|p, name| p == pid && name.eq_ignore_ascii_case(exe_name)).is_some()
}

/// A handle with only the `SYNCHRONIZE` right, for waiting on the end of a
/// process.
#[derive(Debug)]
pub struct WaitHandle(HANDLE);

impl WaitHandle {
    pub fn open(pid: u32) -> windows::core::Result<Self> {
        // SAFETY: only SYNCHRONIZE, no inheritance.
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.map(Self)
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for WaitHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
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
