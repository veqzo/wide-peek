//! Hidden top-level windows and a wait that keeps the message queue alive.
//!
//! A GUI-subsystem process gets `WM_QUERYENDSESSION` and `WM_ENDSESSION` only
//! through a top-level window. Message-only windows do not get them.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassExW,
    TranslateMessage, WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSEXW, WNDPROC, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR};

/// A hidden top-level window. Drop destroys it.
#[derive(Debug)]
pub struct HiddenWindow(HWND);

impl HiddenWindow {
    /// Registers `class` (once per process) and creates an invisible window.
    pub fn create(class: &str, wndproc: WNDPROC) -> anyhow::Result<Self> {
        // SAFETY: NULL gives the handle of the exe.
        let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
        let class_name = HSTRING::from(class);
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: wndproc,
            hInstance: instance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        // SAFETY: `wc` is complete. A second registration fails harmlessly.
        unsafe { RegisterClassExW(&wc) };
        // SAFETY: the class exists; no WS_VISIBLE, so the window stays hidden.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                &class_name,
                &class_name,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance.into()),
                None,
            )
        }
        .with_context(|| format!("cannot create the window {class}"))?;
        Ok(Self(hwnd))
    }

    pub fn hwnd(&self) -> HWND {
        self.0
    }
}

impl Drop for HiddenWindow {
    fn drop(&mut self) {
        // SAFETY: the window belongs to this thread.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// Window procedure for windows that only react to the end of the Windows
/// session: it runs `on_end` when the session really ends.
pub fn end_session_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    on_end: fn(),
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                info!("Windows session ends");
                // A panic must not cross the FFI boundary.
                if std::panic::catch_unwind(on_end).is_err() {
                    error!("restore at session end panicked");
                }
            }
            LRESULT(0)
        }
        // SAFETY: default handling for all other messages.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Handles all waiting window messages of this thread.
pub fn pump_messages() {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid output buffer.
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
        // SAFETY: `msg` comes from PeekMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Sleeps for `duration` and handles window messages while it sleeps.
pub fn sleep(duration: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(());
        }
        let millis = u32::try_from(left.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: no handles; only the message queue.
        let result =
            unsafe { MsgWaitForMultipleObjectsEx(None, millis, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        if result == WAIT_OBJECT_0 {
            pump_messages();
        } else if result == WAIT_TIMEOUT {
            return Ok(());
        } else {
            bail!("waiting failed: {}", windows::core::Error::from_thread());
        }
    }
}
