//! `widepeek tray`: a notification-area icon that stretches the display when
//! `VALORANT.exe` starts and restores it when the game ends.
//!
//! The main thread owns the window, the icon and a 2 s timer. Each tick takes
//! one Toolhelp snapshot while no session runs. A worker thread runs the
//! session, so the menu stays responsive. The launcher is the more reliable
//! path: the tray stretches only after the game process starts.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::time::{Duration, Instant};

use anyhow::Context;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos,
    GetMessageW, GetSystemMetrics, HICON, IMAGE_ICON, KillTimer, LR_DEFAULTCOLOR, LoadImageW,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterWindowMessageW,
    SM_CXSMICON, SM_CYSMICON, SetForegroundWindow, SetTimer, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY, WM_ENDSESSION,
    WM_LBUTTONUP, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_TIMER,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::process;
use crate::session::{self, StretchSession};
use crate::window::{self, HiddenWindow};
use crate::{config, instance, restore, riot, state, ui};

/// The trigger poll. One snapshot costs well below 1 ms of CPU.
const POLL: Duration = Duration::from_secs(2);
const START_POLL: Duration = Duration::from_secs(1);
const EXIT_POLL: Duration = Duration::from_secs(5);
/// The session ends if neither Valorant process runs for this many polls
/// before the game starts (the user cancelled the start).
const GONE_POLLS: u32 = 5;

const TIMER_ID: usize = 1;
const ICON_ID: u32 = 1;
const WM_TRAY_ICON: u32 = WM_APP + 1;
/// Sent by the worker. `wparam` is a `Status` value.
const WM_TRAY_STATUS: u32 = WM_APP + 2;

const CMD_RESTORE: usize = 1;
const CMD_QUIT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Waiting = 0,
    Stretched = 1,
    Restored = 2,
}

impl Status {
    fn from_raw(raw: usize) -> Self {
        match raw {
            1 => Self::Stretched,
            2 => Self::Restored,
            _ => Self::Waiting,
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::Waiting => "Waiting for Valorant",
            Self::Stretched => "Stretched: Valorant runs",
            Self::Restored => "Restored: waiting for Valorant to end",
        }
    }
}

/// State of the main thread. Only `Cell`s, because the menu runs a modal
/// loop that calls the window procedure again.
struct Tray {
    icon: Cell<HICON>,
    taskbar_created: Cell<u32>,
    busy: Cell<bool>,
    was_running: Cell<bool>,
    status: Cell<Status>,
}

thread_local! {
    static TRAY: Tray = const {
        Tray {
            icon: Cell::new(HICON(std::ptr::null_mut())),
            taskbar_created: Cell::new(0),
            busy: Cell::new(false),
            was_running: Cell::new(false),
            status: Cell::new(Status::Waiting),
        }
    };
    /// Last error of the worker, shown in the menu.
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub fn run() -> anyhow::Result<()> {
    let Some(_lock) = instance::try_acquire(instance::TRAY)? else {
        info!("the tray already runs");
        return Ok(());
    };
    // Fail early with a clear message if setup did not run.
    config::load_required()?;
    if state::exists() {
        restore::run(false).context("cannot restore the last session")?;
    }

    let window = HiddenWindow::create("widepeek-tray", Some(tray_proc))?;
    let hwnd = window.hwnd();
    TRAY.with(|t| {
        t.icon.set(load_icon());
        // SAFETY: constant string.
        t.taskbar_created
            .set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
        // An already running game does not trigger a session.
        t.was_running
            .set(process::find(riot::BOOTSTRAP_PROCESS).is_some());
    });
    notify(hwnd, NIM_ADD)?;
    // SAFETY: the window belongs to this thread.
    if unsafe { SetTimer(Some(hwnd), TIMER_ID, POLL.as_millis() as u32, None) } == 0 {
        bail_remove(hwnd)?;
    }
    info!("tray started");
    // Startup pages (config parser, icon loading) are not needed again.
    // SAFETY: (-1, -1) only trims the working set of this process.
    let _ = unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };

    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns the window.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    info!("tray ended");
    drop(window);
    Ok(())
}

fn bail_remove(hwnd: HWND) -> anyhow::Result<()> {
    let _ = notify(hwnd, NIM_DELETE);
    anyhow::bail!(
        "cannot start the tray timer: {}",
        windows::core::Error::from_thread()
    )
}

/// The small icon from the exe resources (ID 1).
fn load_icon() -> HICON {
    // SAFETY: NULL gives the handle of the exe; ID 1 is in res/widepeek.rc.
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
            return HICON::default();
        };
        LoadImageW(
            Some(module.into()),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        )
        .map_or_else(|_| HICON::default(), |h| HICON(h.0))
    }
}

fn notify(hwnd: HWND, message: NOTIFY_ICON_MESSAGE) -> anyhow::Result<()> {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_ICON,
        hIcon: TRAY.with(|t| t.icon.get()),
        ..Default::default()
    };
    let tip = format!("widepeek: {}", TRAY.with(|t| t.status.get()).text());
    for (dst, src) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *dst = src;
    }
    // SAFETY: `data` is complete and has the right size.
    if unsafe { Shell_NotifyIconW(message, &data) }.as_bool() {
        Ok(())
    } else {
        anyhow::bail!("cannot update the notification icon")
    }
}

fn set_status(hwnd: HWND, status: Status) {
    TRAY.with(|t| t.status.set(status));
    let _ = notify(hwnd, NIM_MODIFY);
}

extern "system" fn tray_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == TIMER_ID => {
            on_timer(hwnd);
            LRESULT(0)
        }
        WM_TRAY_ICON => {
            let mouse = (lparam.0 & 0xffff) as u32;
            if mouse == WM_RBUTTONUP || mouse == WM_LBUTTONUP {
                show_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_TRAY_STATUS => {
            let status = Status::from_raw(wparam.0);
            if status == Status::Waiting {
                TRAY.with(|t| t.busy.set(false));
            }
            // A manual restore stays visible until the game ends.
            let shown = TRAY.with(|t| t.status.get());
            if !(shown == Status::Restored && status == Status::Stretched) {
                set_status(hwnd, status);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            quit(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: the timer and the icon belong to this window.
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_ID) };
            let _ = notify(hwnd, NIM_DELETE);
            // SAFETY: ends the message loop of this thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_QUERYENDSESSION | WM_ENDSESSION => {
            window::end_session_proc(hwnd, msg, wparam, lparam, close_session)
        }
        _ if msg != 0 && msg == TRAY.with(|t| t.taskbar_created.get()) => {
            // Explorer restarted: add the icon again.
            let _ = notify(hwnd, NIM_ADD);
            LRESULT(0)
        }
        // SAFETY: default handling for all other messages.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn close_session() {
    let _ = session::close();
}

/// Starts a session on the rising edge of `VALORANT.exe`.
fn on_timer(hwnd: HWND) {
    TRAY.with(|t| {
        if t.busy.get() {
            // No snapshot while a session runs. After it, a still running
            // VALORANT.exe must not start a second session.
            t.was_running.set(true);
            return;
        }
        let running = process::find(riot::BOOTSTRAP_PROCESS).is_some();
        let rising = running && !t.was_running.get();
        t.was_running.set(running);
        if !rising {
            return;
        }
        // The launcher owns this start.
        match instance::try_acquire(instance::LAUNCH) {
            Ok(Some(lock)) => drop(lock),
            Ok(None) => {
                info!("{} started by the launcher", riot::BOOTSTRAP_PROCESS);
                return;
            }
            Err(err) => warn!("cannot check the launcher: {err:#}"),
        }
        info!("{} started, stretching", riot::BOOTSTRAP_PROCESS);
        t.busy.set(true);
        let raw = hwnd.0 as isize;
        let spawned = std::thread::Builder::new()
            .name("session".into())
            .spawn(move || worker(raw));
        if let Err(err) = spawned {
            error!("cannot start the session thread: {err}");
            t.busy.set(false);
        }
    });
}

/// Runs one session. Always posts `Waiting` at the end.
fn worker(raw_hwnd: isize) {
    let post = |status: Status| {
        let hwnd = HWND(raw_hwnd as *mut c_void);
        // SAFETY: PostMessageW is safe from any thread; a closed window only
        // makes the call fail.
        let _ = unsafe {
            PostMessageW(
                Some(hwnd),
                WM_TRAY_STATUS,
                WPARAM(status as usize),
                LPARAM(0),
            )
        };
    };
    let result = std::panic::catch_unwind(|| run_session(|| post(Status::Stretched)));
    let message = match result {
        Ok(Ok(())) => None,
        Ok(Err(err)) => Some(format!("{err:#}")),
        Err(_) => Some("the session thread panicked; the display is restored".to_owned()),
    };
    if let Some(message) = &message {
        error!("tray session failed: {message}");
    }
    post(Status::Waiting);
    if let Some(message) = message {
        ui::error(&message);
    }
}

fn run_session(on_stretched: impl Fn()) -> anyhow::Result<()> {
    let config = config::load_required()?;
    let session = StretchSession::begin(&config)?;
    on_stretched();
    let timeout = Duration::from_secs(config.start_timeout_secs);
    let Some(pid) = wait_for_game(timeout)? else {
        info!("Valorant ended before the game started");
        return session.finish();
    };
    info!("{} started (pid {pid})", riot::GAME_PROCESS);
    wait_for_exit(pid);
    info!("{} ended", riot::GAME_PROCESS);
    session.finish()
}

/// Polls for the game process. `None` if the start was cancelled.
fn wait_for_game(timeout: Duration) -> anyhow::Result<Option<u32>> {
    let deadline = Instant::now() + timeout;
    let mut gone = 0;
    loop {
        if let Some(pid) = process::find(riot::GAME_PROCESS) {
            return Ok(Some(pid));
        }
        if process::find(riot::BOOTSTRAP_PROCESS).is_some() {
            gone = 0;
        } else {
            gone += 1;
            if gone >= GONE_POLLS {
                return Ok(None);
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "{} did not start within {} s",
                riot::GAME_PROCESS,
                timeout.as_secs()
            );
        }
        std::thread::sleep(START_POLL);
    }
}

/// Polls the process list until the game ends. widepeek never opens a
/// handle to the game process (anti-cheat rule).
fn wait_for_exit(pid: u32) {
    while process::is_running(pid, riot::GAME_PROCESS) {
        std::thread::sleep(EXIT_POLL);
    }
}

fn show_menu(hwnd: HWND) {
    let status = TRAY.with(|t| t.status.get());
    let error = LAST_ERROR.with(|e| e.borrow().clone());
    let label = HSTRING::from(match &error {
        Some(error) => format!("Error: {error}"),
        None => status.text().to_owned(),
    });
    // SAFETY: the menu lives until DestroyMenu below; strings live until
    // TrackPopupMenu returns.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &label);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_RESTORE, w!("Restore now"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        // Without this, the menu does not close on a click outside.
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match command.0 as usize {
            CMD_RESTORE => restore_now(hwnd),
            CMD_QUIT => quit(hwnd),
            _ => {}
        }
    }
}

fn restore_now(hwnd: HWND) {
    let result = match session::restore_active() {
        Some(result) => result,
        None => restore::run(false),
    };
    match result {
        Ok(()) => {
            LAST_ERROR.with(|e| e.borrow_mut().take());
            if TRAY.with(|t| t.busy.get()) {
                set_status(hwnd, Status::Restored);
            }
        }
        Err(err) => {
            let text = format!("{err:#}");
            LAST_ERROR.with(|e| *e.borrow_mut() = Some(text.clone()));
            ui::error(&text);
        }
    }
}

fn quit(hwnd: HWND) {
    if let Some(Err(err)) = session::close() {
        ui::error(&format!("{err:#}"));
    }
    // SAFETY: the window belongs to this thread.
    let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd) };
}
