//! Session state in `%LocalAppData%\widepeek\state.json`.
//!
//! The file exists only while the display is stretched. If it is still there
//! at the next start or at logon, the last session did not end cleanly and
//! widepeek restores the display first.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::display::DisplayMode;
use crate::paths;

const STATE_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub pid: u32,
    pub started_at_unix: u64,
    pub display_name: String,
    pub native: DisplayMode,
    pub stretch: DisplayMode,
    /// The monitor devices that the session disables.
    #[serde(default)]
    pub monitor_instance_ids: Vec<String>,
    /// The other active displays and their modes at the session start.
    #[serde(default)]
    pub other_displays: Vec<SavedDisplay>,
    /// Version 1 stored one monitor device. Read only.
    #[serde(default, rename = "monitor_instance_id", skip_serializing)]
    legacy_monitor_instance_id: Option<String>,
}

/// A display and the mode that restore sets back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDisplay {
    pub name: String,
    pub mode: DisplayMode,
}

impl State {
    pub fn new(
        display_name: String,
        native: DisplayMode,
        stretch: DisplayMode,
        monitor_instance_ids: Vec<String>,
        other_displays: Vec<SavedDisplay>,
    ) -> Self {
        let started_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self {
            version: STATE_VERSION,
            pid: std::process::id(),
            started_at_unix,
            display_name,
            native,
            stretch,
            monitor_instance_ids,
            other_displays,
            legacy_monitor_instance_id: None,
        }
    }

    /// All monitor devices that restore must enable, also from a version 1 file.
    pub fn monitors(&self) -> Vec<String> {
        let mut ids = self.monitor_instance_ids.clone();
        if let Some(id) = &self.legacy_monitor_instance_id
            && !ids.iter().any(|known| known.eq_ignore_ascii_case(id))
        {
            ids.push(id.clone());
        }
        ids
    }
}

/// Reads the state. A missing file gives `None`.
pub fn load() -> anyhow::Result<Option<State>> {
    let path = paths::state_file()?;
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not valid", path.display()))
            .map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Reads the state for restore. A broken file gives a warning and `None`, so
/// restore falls back to the config or to the best available mode.
pub fn load_lenient() -> Option<State> {
    match load() {
        Ok(state) => state,
        Err(err) => {
            warn!("ignoring state: {err:#}");
            None
        }
    }
}

/// Returns true if a state file exists, also when it is broken.
pub fn exists() -> bool {
    paths::state_file().is_ok_and(|p| p.exists())
}

pub fn save(state: &State) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(state)?;
    paths::write_atomic(&paths::state_file()?, &json)
}

pub fn clear() -> anyhow::Result<()> {
    paths::remove_if_exists(&paths::state_file()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let native = DisplayMode {
            width: 2560,
            height: 1440,
            hz: 144,
            bpp: 32,
        };
        let stretch = DisplayMode {
            width: 2088,
            ..native
        };
        let state = State::new(
            r"\\.\DISPLAY1".to_owned(),
            native,
            stretch,
            vec!["ID1".to_owned(), "ID2".to_owned()],
            vec![SavedDisplay {
                name: r"\\.\DISPLAY2".to_owned(),
                mode: native,
            }],
        );
        let json = serde_json::to_vec(&state).unwrap();
        assert_eq!(serde_json::from_slice::<State>(&json).unwrap(), state);
    }

    #[test]
    fn reads_version_1() {
        let json = r#"{"version":1,"pid":1,"started_at_unix":0,
            "display_name":"\\\\.\\DISPLAY1",
            "native":{"width":2560,"height":1440,"hz":144,"bpp":32},
            "stretch":{"width":2088,"height":1440,"hz":144,"bpp":32},
            "monitor_instance_id":"ID"}"#;
        let state: State = serde_json::from_str(json).unwrap();
        assert_eq!(state.monitors(), vec!["ID".to_owned()]);
        assert!(state.other_displays.is_empty());
    }
}
