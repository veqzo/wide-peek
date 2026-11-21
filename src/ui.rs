//! Simple message boxes for the modes without a console.

use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IDNO, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING,
    MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MESSAGEBOX_STYLE, MessageBoxW, PostMessageW,
    WM_COMMAND,
};
use windows::core::{HSTRING, PCWSTR, w};

fn show(text: &str, style: MESSAGEBOX_STYLE) -> bool {
    // SAFETY: both strings live until the call returns.
    let result = unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            w!("widepeek"),
            style | MB_SETFOREGROUND,
        )
    };
    result == IDYES
}

pub fn error(text: &str) {
    show(text, MB_OK | MB_ICONERROR);
}

pub fn info(text: &str) {
    show(text, MB_OK | MB_ICONINFORMATION);
}

/// Asks a yes/no question. Returns true for "Yes".
pub fn ask(text: &str) -> bool {
    show(text, MB_YESNO | MB_ICONWARNING)
}

/// Asks a yes/no question that answers "No" by itself after `timeout`.
pub fn ask_with_timeout(text: &str, timeout: Duration) -> bool {
    const TITLE: PCWSTR = w!("widepeek - keep this resolution?");
    let (tx, rx) = mpsc::channel();
    let text = HSTRING::from(text);
    std::thread::spawn(move || {
        // SAFETY: both strings live until the call returns.
        let result = unsafe {
            MessageBoxW(
                None,
                &text,
                TITLE,
                MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST,
            )
        };
        let _ = tx.send(result == IDYES);
    });
    match rx.recv_timeout(timeout) {
        Ok(yes) => yes,
        Err(_) => {
            // Close the box, so the thread ends. Never wait for it.
            // SAFETY: plain window lookup and a posted message.
            if let Ok(hwnd) = unsafe { FindWindowW(w!("#32770"), TITLE) } {
                let _ = unsafe {
                    PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(IDNO.0 as usize), LPARAM(0))
                };
            }
            info!("no answer within {} s, answering no", timeout.as_secs());
            false
        }
    }
}
