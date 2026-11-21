//! widepeek: starts Valorant in true stretched resolution and always restores
//! the native display mode afterwards.

#![windows_subsystem = "windows"]

#[macro_use]
mod log;

mod autostart;
mod cli;
mod com;
mod config;
mod console;
mod customres;
mod display;
mod elevation;
mod instance;
mod launcher;
mod monitor;
mod paths;
mod process;
mod restore;
mod riot;
mod session;
mod setup;
mod shortcut;
mod state;
mod system;
mod tray;
mod ui;
mod uninstall;
mod wide;
mod window;

use std::process::ExitCode;

use cli::Command;

fn main() -> ExitCode {
    let dll_search = restrict_dll_search();
    let command = match cli::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(err) => {
            console::attach_parent();
            console::eprintln(&format!("error: {err}\n\n{}", cli::USAGE));
            return ExitCode::from(2);
        }
    };

    if command.uses_parent_console() {
        console::attach_parent();
    }
    log::init(command.name(), command.writes_log_file());
    install_panic_hook();
    if let Err(err) = dll_search {
        warn!("cannot restrict the DLL search path: {err}");
    }

    match run(&command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("{} failed: {err:#}", command.name());
            report_error(&command, &err);
            ExitCode::FAILURE
        }
    }
}

fn run(command: &Command) -> anyhow::Result<()> {
    match command {
        Command::Launch => launcher::run(),
        Command::Tray => tray::run(),
        Command::Setup(args) => setup::run(args),
        Command::Restore { at_logon } => restore::run(*at_logon),
        Command::MonitorOff => monitor::command(false),
        Command::MonitorOn => monitor::command(true),
        Command::Uninstall { yes } => uninstall::run(*yes),
        Command::InstallSystem { user_sid } => system::install_here(user_sid).map(drop),
        Command::UninstallSystem => system::uninstall_here(),
        Command::Help => {
            console::println(cli::USAGE);
            Ok(())
        }
        Command::Version => {
            console::println(concat!("widepeek ", env!("CARGO_PKG_VERSION")));
            Ok(())
        }
    }
}

/// Shows the error where the user can see it: the console if there is one,
/// otherwise a message box for the interactive modes. Task modes only log.
fn report_error(command: &Command, err: &anyhow::Error) {
    // Setup shows its error itself, before it waits for Enter.
    if matches!(command, Command::Setup(_)) {
        return;
    }
    if console::has_console() {
        console::eprintln(&format!("error: {err:#}"));
    } else if command.is_interactive() {
        ui::error(&format!("{err:#}"));
    }
}

/// Later loads (for example `dbghelp.dll` for a panic backtrace) search
/// only System32, never the folder of the exe or the working directory.
fn restrict_dll_search() -> windows::core::Result<()> {
    use windows::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
    };
    // SAFETY: plain flag call without pointers.
    unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) }
}

/// Logs panics. Unwinding still runs the drop guards that restore the display.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error!("panic: {info}");
        default_hook(info);
    }));
}
