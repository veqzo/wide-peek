//! Administrator rights: check them and start an elevated copy via UAC.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

/// Returns true if the process token is elevated.
pub fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle of the current process is always valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: the buffer is a TOKEN_ELEVATION with the size we pass.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    }
    .is_ok();
    // SAFETY: `token` is a handle that we own.
    let _ = unsafe { CloseHandle(token) };
    ok && elevation.TokenIsElevated != 0
}

/// String SID of the user of this process, for example `S-1-5-21-...`.
pub fn current_user_sid() -> anyhow::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle of the current process is always valid.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .context("cannot open the process token")?;
    let result = user_sid_of(token);
    // SAFETY: `token` is a handle that we own.
    let _ = unsafe { CloseHandle(token) };
    result
}

fn user_sid_of(token: HANDLE) -> anyhow::Result<String> {
    let mut len = 0u32;
    // SAFETY: a size query without a buffer. It fails with the needed size.
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    // u64 elements give the alignment that TOKEN_USER needs.
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: the buffer has at least `len` bytes.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
    }
    .context("cannot read the user of the process token")?;
    // SAFETY: the buffer now starts with a TOKEN_USER.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let mut text = PWSTR::null();
    // SAFETY: `sid` points into `buf`, which is still alive.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.context("cannot convert the user SID")?;
    // SAFETY: the system allocated `text` with LocalAlloc.
    let value = unsafe { text.to_string() };
    // SAFETY: we free the string exactly once.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    value.context("the user SID is not valid UTF-16")
}

pub fn current_exe() -> anyhow::Result<PathBuf> {
    std::env::current_exe().context("cannot find the path of widepeek.exe")
}

/// Starts `exe` with the UAC prompt, waits for it and returns its exit code.
pub fn run_elevated(exe: &Path, args: &[String]) -> anyhow::Result<u32> {
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(join_args(args));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: all strings live until the call returns.
    if let Err(err) = unsafe { ShellExecuteExW(&mut info) } {
        if err.code() == ERROR_CANCELLED.to_hresult() {
            bail!("the administrator prompt was cancelled");
        }
        return Err(err).context("cannot start widepeek as administrator");
    }
    let process = info.hProcess;
    if process.is_invalid() {
        bail!("the elevated process did not start");
    }
    let mut code = 1u32;
    // SAFETY: `process` is a process handle that we own.
    unsafe {
        WaitForSingleObject(process, INFINITE);
        let result = GetExitCodeProcess(process, &mut code);
        let _ = CloseHandle(process);
        result.context("cannot read the exit code of the elevated process")?;
    }
    Ok(code)
}

/// Builds a command line with the quoting rules of `CommandLineToArgvW`.
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_arguments() {
        assert_eq!(quote_arg("setup"), "setup");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(r"\\.\DISPLAY1"), r"\\.\DISPLAY1");
        assert_eq!(quote_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote_arg(r#"say "hi""#), r#""say \"hi\"""#);
    }
}
