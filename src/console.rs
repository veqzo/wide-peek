//! Console output for a GUI-subsystem binary.
//!
//! `widepeek.exe` has no console of its own. CLI commands attach to the
//! console of the shell that started them. The interactive setup opens a
//! console of its own, because a shell that shares its console would read the
//! same keyboard input.

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::os::windows::io::IntoRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, FreeConsole, GetStdHandle,
    STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleTitleW,
    SetStdHandle,
};
use windows::core::HSTRING;

static HAS_CONSOLE: AtomicBool = AtomicBool::new(false);

/// Attaches to the console of the parent process, if it has one. Output that
/// is already redirected to a file or pipe stays as it is.
pub fn attach_parent() {
    if std_handle_valid(STD_OUTPUT_HANDLE) {
        HAS_CONSOLE.store(true, Ordering::Relaxed);
        return;
    }
    // SAFETY: plain Win32 call without pointers.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
        bind_console_handles();
        HAS_CONSOLE.store(true, Ordering::Relaxed);
    }
}

/// Opens a new console window for an interactive wizard.
pub fn alloc(title: &str) -> anyhow::Result<()> {
    // SAFETY: plain Win32 calls. FreeConsole fails harmlessly without a console.
    unsafe {
        let _ = FreeConsole();
        AllocConsole()?;
        SetConsoleTitleW(&HSTRING::from(title))?;
    }
    bind_console_handles();
    HAS_CONSOLE.store(true, Ordering::Relaxed);
    Ok(())
}

pub fn has_console() -> bool {
    HAS_CONSOLE.load(Ordering::Relaxed)
}

pub fn println(text: &str) {
    // Output errors (for example a closed pipe) must not stop the tool.
    let _ = writeln!(std::io::stdout(), "{text}");
}

pub fn eprintln(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}

/// Prints a prompt and reads one line. Returns the trimmed input.
pub fn prompt(text: &str) -> anyhow::Result<String> {
    let mut out = std::io::stdout();
    write!(out, "{text}")?;
    out.flush()?;
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line)?;
    if read == 0 {
        anyhow::bail!("no input available");
    }
    Ok(line.trim().to_owned())
}

/// Asks a yes/no question. An empty answer selects `default`.
pub fn confirm(question: &str, default: bool) -> anyhow::Result<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    loop {
        let answer = prompt(&format!("{question} {hint} "))?;
        match answer.to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" | "j" | "ja" => return Ok(true),
            "n" | "no" | "nein" => return Ok(false),
            _ => println("Please answer y or n."),
        }
    }
}

fn std_handle_valid(kind: STD_HANDLE) -> bool {
    // SAFETY: plain Win32 call without pointers.
    unsafe { GetStdHandle(kind) }.is_ok_and(|h| !h.is_invalid() && !h.0.is_null())
}

/// Some Windows versions leave the standard handles empty after
/// `AttachConsole`. Opening `CONIN$`/`CONOUT$` binds them explicitly. The
/// Rust standard library reads the handles on every call, so `stdout()` then
/// writes to the console with correct Unicode output.
fn bind_console_handles() {
    let targets = [
        (STD_INPUT_HANDLE, "CONIN$"),
        (STD_OUTPUT_HANDLE, "CONOUT$"),
        (STD_ERROR_HANDLE, "CONOUT$"),
    ];
    for (kind, device) in targets {
        if std_handle_valid(kind) {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(device) else {
            continue;
        };
        // The handle stays open for the whole process lifetime on purpose.
        let handle = HANDLE(file.into_raw_handle());
        // SAFETY: `handle` is a valid console handle that we own.
        let _ = unsafe { SetStdHandle(kind, handle) };
    }
}
