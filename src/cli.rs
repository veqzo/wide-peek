//! Minimal command line parser. No external crate keeps the binary small.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

pub use crate::update::INSTALL_UPDATE;
use crate::update::Version;

pub const USAGE: &str = r"widepeek - Valorant in true stretched resolution

USAGE:
    widepeek [COMMAND]

COMMANDS:
    launch       Stretch the display, start Valorant, restore afterwards (default)
    tray         Run in the tray and stretch when Valorant starts
    setup        One-time setup (asks for administrator rights)
    restore      Restore the native resolution and enable the monitor
    update       Check for a new version and install it
    uninstall    Restore, then remove tasks, shortcuts, files and configuration
    help         Show this text
    version      Show the version

SETUP OPTIONS:
    --width <px>       Stretched width (default: height x 1.45, multiple of 8)
    --height <px>      Stretched height (default: native height)
    --hz <hz>          Refresh rate (default: native refresh rate)
    --display <name>   GDI display name, for example \\.\DISPLAY1
    --yes              Accept all defaults without questions";

/// Hidden command: the elevated part of setup. Its argument is the SID of
/// the user that started setup.
pub const INSTALL_SYSTEM: &str = "install-system";
/// Hidden command: the elevated part of uninstall.
pub const UNINSTALL_SYSTEM: &str = "uninstall-system";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupArgs {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hz: Option<u32>,
    pub display: Option<String>,
    pub yes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Launch,
    Tray,
    Setup(SetupArgs),
    Restore {
        at_logon: bool,
    },
    MonitorOff,
    MonitorOn,
    Uninstall {
        yes: bool,
    },
    Update,
    InstallSystem {
        user_sid: String,
    },
    InstallUpdate {
        path: PathBuf,
        version: String,
        user_sid: String,
    },
    UninstallSystem,
    Help,
    Version,
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::Tray => "tray",
            Self::Setup(_) => "setup",
            Self::Restore { .. } => "restore",
            Self::MonitorOff => "monitor-off",
            Self::MonitorOn => "monitor-on",
            Self::Uninstall { .. } => "uninstall",
            Self::Update => "update",
            Self::InstallUpdate { .. } => INSTALL_UPDATE,
            Self::InstallSystem { .. } => INSTALL_SYSTEM,
            Self::UninstallSystem => UNINSTALL_SYSTEM,
            Self::Help => "help",
            Self::Version => "version",
        }
    }

    /// Commands that print to the console that started them. The tray stays
    /// detached so that closing the console does not end it. Commands that
    /// ask questions stay detached too: the shell does not wait for a GUI
    /// binary, so it would read the same keyboard input. Setup opens a console
    /// of its own, uninstall asks in a dialog.
    pub fn uses_parent_console(&self) -> bool {
        !matches!(
            self,
            Self::Tray | Self::Setup(SetupArgs { yes: false, .. }) | Self::Uninstall { yes: false }
        )
    }

    /// The elevated commands do not write into the user-writable log
    /// directory, so a planted link there cannot redirect an elevated write.
    /// Their unelevated caller logs the result.
    pub fn writes_log_file(&self) -> bool {
        !matches!(
            self,
            Self::MonitorOff
                | Self::MonitorOn
                | Self::InstallSystem { .. }
                | Self::InstallUpdate { .. }
                | Self::UninstallSystem
                | Self::Help
                | Self::Version
        )
    }

    /// Commands that a person starts directly and that show errors in a dialog.
    pub fn is_interactive(&self) -> bool {
        matches!(
            self,
            Self::Launch
                | Self::Tray
                | Self::Setup(_)
                | Self::Restore { at_logon: false }
                | Self::Uninstall { .. }
                | Self::Update
                | Self::InstallSystem { .. }
                | Self::InstallUpdate { .. }
                | Self::UninstallSystem
        )
    }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> anyhow::Result<Command> {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        return Ok(Command::Launch);
    };
    let rest: Vec<String> = args.collect();
    let command = match first.as_str() {
        "launch" => Command::Launch,
        "tray" => Command::Tray,
        "setup" => return parse_setup(&rest).map(Command::Setup),
        "restore" => {
            let at_logon = match rest.as_slice() {
                [] => false,
                [flag] if flag == "--at-logon" => true,
                _ => bail!("unknown arguments for restore: {}", rest.join(" ")),
            };
            return Ok(Command::Restore { at_logon });
        }
        INSTALL_SYSTEM => {
            let [sid] = rest.as_slice() else {
                bail!("{INSTALL_SYSTEM} needs exactly one user SID");
            };
            if !is_sid(sid) {
                bail!("`{sid}` is not a user SID");
            }
            return Ok(Command::InstallSystem {
                user_sid: sid.clone(),
            });
        }
        INSTALL_UPDATE => return parse_install_update(&rest),
        UNINSTALL_SYSTEM => Command::UninstallSystem,
        "update" => Command::Update,
        "monitor-off" => Command::MonitorOff,
        "monitor-on" => Command::MonitorOn,
        "uninstall" => {
            let yes = match rest.as_slice() {
                [] => false,
                [flag] if flag == "--yes" || flag == "-y" => true,
                _ => bail!("unknown arguments for uninstall: {}", rest.join(" ")),
            };
            return Ok(Command::Uninstall { yes });
        }
        "help" | "--help" | "-h" | "/?" => Command::Help,
        "version" | "--version" | "-V" => Command::Version,
        other => bail!("unknown command `{other}`"),
    };
    if !rest.is_empty() {
        bail!(
            "unknown arguments for {}: {}",
            command.name(),
            rest.join(" ")
        );
    }
    Ok(command)
}

fn parse_setup(args: &[String]) -> anyhow::Result<SetupArgs> {
    let mut setup = SetupArgs::default();
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--yes" | "-y" => setup.yes = true,
            "--width" | "--height" | "--hz" | "--display" => {
                let value = iter
                    .next()
                    .with_context(|| format!("{flag} needs a value"))?;
                match flag.as_str() {
                    "--width" => setup.width = Some(parse_number(flag, value)?),
                    "--height" => setup.height = Some(parse_number(flag, value)?),
                    "--hz" => setup.hz = Some(parse_number(flag, value)?),
                    _ => setup.display = Some(value.clone()),
                }
            }
            other => bail!("unknown setup option `{other}`"),
        }
    }
    Ok(setup)
}

/// `install-update <exe> <version> <sid>`: an absolute `.exe` path on a
/// drive (no UNC or device path), a version `X.Y.Z` and a user SID.
fn parse_install_update(args: &[String]) -> anyhow::Result<Command> {
    let [path_arg, version, sid] = args else {
        bail!("{INSTALL_UPDATE} needs an exe, a version and a user SID");
    };
    let path = Path::new(path_arg);
    let is_exe = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !path.is_absolute() || path_arg.starts_with(r"\\") || !is_exe {
        bail!("`{}` is not an absolute path to an exe", path.display());
    }
    Version::parse(version)?;
    if !is_sid(sid) {
        bail!("`{sid}` is not a user SID");
    }
    Ok(Command::InstallUpdate {
        path: path.to_path_buf(),
        version: version.clone(),
        user_sid: sid.clone(),
    })
}

/// A string SID like `S-1-5-21-...`: only digits and dashes, so it is safe in
/// the task XML and on a command line.
fn is_sid(text: &str) -> bool {
    text.strip_prefix("S-1-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .split('-')
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    })
}

fn parse_number(flag: &str, value: &str) -> anyhow::Result<u32> {
    let number: u32 = value
        .parse()
        .with_context(|| format!("{flag}: `{value}` is not a number"))?;
    if number == 0 {
        bail!("{flag} must be greater than 0");
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> anyhow::Result<Command> {
        parse(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn no_arguments_means_launch() {
        assert_eq!(p(&[]).unwrap(), Command::Launch);
    }

    #[test]
    fn parses_restore_at_logon() {
        assert_eq!(
            p(&["restore", "--at-logon"]).unwrap(),
            Command::Restore { at_logon: true }
        );
        assert!(p(&["restore", "--bogus"]).is_err());
    }

    #[test]
    fn commands_that_ask_questions_do_not_share_the_parent_console() {
        assert!(!p(&["setup"]).unwrap().uses_parent_console());
        assert!(!p(&["uninstall"]).unwrap().uses_parent_console());
        assert!(!p(&["tray"]).unwrap().uses_parent_console());
        assert!(p(&["setup", "--yes"]).unwrap().uses_parent_console());
        assert!(p(&["uninstall", "--yes"]).unwrap().uses_parent_console());
        assert!(p(&["launch"]).unwrap().uses_parent_console());
    }

    #[test]
    fn parses_setup_options_and_round_trips() {
        let cmd = p(&[
            "setup",
            "--width",
            "2088",
            "--hz",
            "144",
            "--display",
            r"\\.\DISPLAY1",
            "--yes",
        ])
        .unwrap();
        let Command::Setup(args) = cmd else {
            panic!("not setup")
        };
        assert_eq!(args.width, Some(2088));
        assert_eq!(args.hz, Some(144));
        assert_eq!(args.display.as_deref(), Some(r"\\.\DISPLAY1"));
        assert!(args.yes);
    }

    #[test]
    fn parses_install_system_with_a_valid_sid_only() {
        assert_eq!(
            p(&["install-system", "S-1-5-21-100-200-300-1001"]).unwrap(),
            Command::InstallSystem {
                user_sid: "S-1-5-21-100-200-300-1001".to_owned()
            }
        );
        assert!(p(&["install-system"]).is_err());
        assert!(p(&["install-system", "S-1-5-21-1<x>"]).is_err());
        assert!(p(&["install-system", "S-1-"]).is_err());
        assert!(p(&["install-system", "S-1-5--1"]).is_err());
    }

    #[test]
    fn parses_update_commands() {
        assert_eq!(p(&["update"]).unwrap(), Command::Update);
        assert!(p(&["update", "now"]).is_err());
        let version = "1.0.0";
        let sid = "S-1-5-21-100-200-300-1001";
        let exe = r"C:\x\widepeek-1.0.0.exe";
        assert_eq!(
            p(&["install-update", exe, version, sid]).unwrap(),
            Command::InstallUpdate {
                path: PathBuf::from(exe),
                version: version.to_owned(),
                user_sid: sid.to_owned(),
            }
        );
        let unc = r"\\server\share\widepeek.exe";
        assert!(p(&["install-update", "widepeek.exe", version, sid]).is_err());
        assert!(p(&["install-update", unc, version, sid]).is_err());
        assert!(p(&["install-update", r"\\?\C:\x\w.exe", version, sid]).is_err());
        assert!(p(&["install-update", r"C:\x\widepeek.dll", version, sid]).is_err());
        assert!(p(&["install-update", exe, "1.0", sid]).is_err());
        assert!(p(&["install-update", exe, version, "S-1-"]).is_err());
        assert!(p(&["install-update", exe, version]).is_err());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(p(&["setup", "--width"]).is_err());
        assert!(p(&["setup", "--width", "abc"]).is_err());
        assert!(p(&["setup", "--width", "0"]).is_err());
        assert!(p(&["launch", "extra"]).is_err());
        assert!(p(&["nope"]).is_err());
    }
}
