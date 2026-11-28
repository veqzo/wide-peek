//! Waits for the start and the end of the game. The launcher and the tray
//! use it.
//!
//! Anti-cheat rule: widepeek never opens a handle to the game process. It
//! only reads the system process list.

use std::time::{Duration, Instant};

use anyhow::bail;

use crate::{process, riot};

const START_POLL: Duration = Duration::from_secs(1);
const EXIT_POLL: Duration = Duration::from_secs(5);
/// The start is cancelled when no start process and no game process runs
/// for this many polls in a row.
const GONE_POLLS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The game process runs with this PID.
    Running(u32),
    /// The user closed the Riot Client or Valorant before the game started.
    Cancelled,
}

/// Polls for the game process once per second.
///
/// `starters` are the processes that run while the game starts, for example
/// the Riot Client. When none of them runs for some seconds, the start is
/// cancelled, so the display does not stay stretched until the timeout.
/// `sleep` waits between two polls.
pub fn wait_for_start(
    timeout: Duration,
    starters: &[&str],
    mut sleep: impl FnMut(Duration) -> anyhow::Result<()>,
) -> anyhow::Result<Start> {
    let deadline = Instant::now() + timeout;
    let names: Vec<&str> = std::iter::once(riot::GAME_PROCESS)
        .chain(starters.iter().copied())
        .collect();
    let mut watch = StartWatch::default();
    loop {
        // One snapshot for all names.
        let found = process::find_each(&names);
        let starting = found[1..].iter().any(Option::is_some);
        if let Some(start) = watch.observe(found[0], starting) {
            return Ok(start);
        }
        if Instant::now() >= deadline {
            bail!(
                "{} did not start within {} s. If the Riot Client installs an update, \
                 wait until the update ends, then start widepeek again. Or increase \
                 start_timeout_secs in the config.",
                riot::GAME_PROCESS,
                timeout.as_secs()
            );
        }
        sleep(START_POLL)?;
    }
}

/// Polls the process list until the game ends.
pub fn wait_for_exit(
    pid: u32,
    mut sleep: impl FnMut(Duration) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    while process::is_running(pid, riot::GAME_PROCESS) {
        sleep(EXIT_POLL)?;
    }
    Ok(())
}

/// The decision of `wait_for_start` for one poll, without the process list.
#[derive(Debug, Default)]
struct StartWatch {
    gone: u32,
}

impl StartWatch {
    fn observe(&mut self, game: Option<u32>, starting: bool) -> Option<Start> {
        if let Some(pid) = game {
            return Some(Start::Running(pid));
        }
        if starting {
            self.gone = 0;
            return None;
        }
        self.gone += 1;
        (self.gone >= GONE_POLLS).then_some(Start::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_game_process_wins() {
        let mut watch = StartWatch::default();
        assert_eq!(watch.observe(Some(7), false), Some(Start::Running(7)));
        assert_eq!(watch.observe(Some(7), true), Some(Start::Running(7)));
    }

    #[test]
    fn waits_while_a_start_process_runs() {
        let mut watch = StartWatch::default();
        for _ in 0..100 {
            assert_eq!(watch.observe(None, true), None);
        }
    }

    #[test]
    fn cancels_after_the_start_processes_are_gone() {
        let mut watch = StartWatch::default();
        for _ in 1..GONE_POLLS {
            assert_eq!(watch.observe(None, false), None);
        }
        assert_eq!(watch.observe(None, false), Some(Start::Cancelled));
    }

    #[test]
    fn a_start_process_resets_the_count() {
        let mut watch = StartWatch::default();
        for _ in 1..GONE_POLLS {
            watch.observe(None, false);
        }
        assert_eq!(watch.observe(None, true), None);
        for _ in 1..GONE_POLLS {
            assert_eq!(watch.observe(None, false), None);
        }
        assert_eq!(watch.observe(None, false), Some(Start::Cancelled));
    }
}
