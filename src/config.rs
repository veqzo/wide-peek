//! User configuration in `%AppData%\widepeek\config.toml`. Setup writes it.

use std::path::PathBuf;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::display::DisplayMode;
use crate::paths;

pub const DEFAULT_START_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// GDI name of the display, for example `\\.\DISPLAY1`.
    pub display_name: String,
    /// Device instance ID of the Monitor-class device that is disabled while
    /// the game runs, for example `DISPLAY\GSM5B7F\5&2a3b&0&UID4352`.
    pub monitor_instance_id: String,
    pub stretch_width: u32,
    pub stretch_height: u32,
    pub refresh_hz: u32,
    pub native_width: u32,
    pub native_height: u32,
    pub native_hz: u32,
    pub native_bpp: u32,
    #[serde(default = "default_true")]
    pub toggle_monitor: bool,
    /// Overrides the Riot Client path from `RiotClientInstalls.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub riot_client_path: Option<PathBuf>,
    #[serde(default = "default_start_timeout")]
    pub start_timeout_secs: u64,
    #[serde(default)]
    pub tray_autostart: bool,
}

fn default_true() -> bool {
    true
}

fn default_start_timeout() -> u64 {
    DEFAULT_START_TIMEOUT_SECS
}

impl Config {
    pub fn native_mode(&self) -> DisplayMode {
        DisplayMode {
            width: self.native_width,
            height: self.native_height,
            hz: self.native_hz,
            bpp: self.native_bpp,
        }
    }

    pub fn stretch_mode(&self) -> DisplayMode {
        DisplayMode {
            width: self.stretch_width,
            height: self.stretch_height,
            hz: self.refresh_hz,
            bpp: self.native_bpp,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let values = [
            ("stretch_width", self.stretch_width),
            ("stretch_height", self.stretch_height),
            ("refresh_hz", self.refresh_hz),
            ("native_width", self.native_width),
            ("native_height", self.native_height),
            ("native_hz", self.native_hz),
            ("native_bpp", self.native_bpp),
        ];
        for (name, value) in values {
            if value == 0 {
                bail!("{name} must be greater than 0");
            }
        }
        if self.refresh_hz > self.native_hz {
            bail!(
                "refresh_hz ({}) must not be above native_hz ({})",
                self.refresh_hz,
                self.native_hz
            );
        }
        if self.display_name.is_empty() {
            bail!("display_name is empty");
        }
        if self.toggle_monitor && self.monitor_instance_id.is_empty() {
            bail!("monitor_instance_id is empty, but toggle_monitor is true");
        }
        if !(10..=3600).contains(&self.start_timeout_secs) {
            bail!("start_timeout_secs must be between 10 and 3600");
        }
        Ok(())
    }

    pub fn to_toml(&self) -> anyhow::Result<String> {
        let body = toml::to_string_pretty(self).context("cannot serialize the config")?;
        Ok(format!(
            "# widepeek configuration. `widepeek setup` writes this file.\n\
             # Run `widepeek setup` again after you change the stretched resolution.\n\n{body}"
        ))
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(text).context("config.toml is not valid")?;
        config.validate().context("config.toml is not valid")?;
        Ok(config)
    }
}

/// Reads the config. A missing file gives `None`.
pub fn load() -> anyhow::Result<Option<Config>> {
    let path = paths::config_file()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Config::from_toml(&text)
            .with_context(|| format!("in {}", path.display()))
            .map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Reads the config and fails with a clear message if setup did not run yet.
pub fn load_required() -> anyhow::Result<Config> {
    load()?.context("widepeek is not set up yet. Run `widepeek setup` first.")
}

/// Reads the config for restore. Restore must also work with a broken
/// config, so an invalid file only gives a warning.
pub fn load_lenient() -> Option<Config> {
    match load() {
        Ok(config) => config,
        Err(err) => {
            warn!("ignoring config: {err:#}");
            None
        }
    }
}

pub fn save(config: &Config) -> anyhow::Result<()> {
    config.validate()?;
    paths::write_atomic(&paths::config_file()?, config.to_toml()?.as_bytes())
}

#[cfg(test)]
pub fn sample() -> Config {
    Config {
        display_name: r"\\.\DISPLAY1".to_owned(),
        monitor_instance_id: r"DISPLAY\GSM5B7F\5&2a3b&0&UID4352".to_owned(),
        stretch_width: 2088,
        stretch_height: 1440,
        refresh_hz: 144,
        native_width: 2560,
        native_height: 1440,
        native_hz: 144,
        native_bpp: 32,
        toggle_monitor: true,
        riot_client_path: None,
        start_timeout_secs: DEFAULT_START_TIMEOUT_SECS,
        tray_autostart: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let mut config = sample();
        config.riot_client_path = Some(PathBuf::from(
            r"D:\Riot Games\Riot Client\RiotClientServices.exe",
        ));
        let text = config.to_toml().unwrap();
        assert_eq!(Config::from_toml(&text).unwrap(), config);
    }

    #[test]
    fn fills_defaults() {
        let text = sample().to_toml().unwrap();
        let reduced: String = text
            .lines()
            .filter(|l| !l.starts_with("toggle_monitor") && !l.starts_with("start_timeout_secs"))
            .map(|l| format!("{l}\n"))
            .collect();
        let config = Config::from_toml(&reduced).unwrap();
        assert!(config.toggle_monitor);
        assert_eq!(config.start_timeout_secs, DEFAULT_START_TIMEOUT_SECS);
    }

    #[test]
    fn rejects_refresh_above_native() {
        let mut config = sample();
        config.refresh_hz = 165;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_zero_values() {
        let mut config = sample();
        config.stretch_width = 0;
        assert!(config.validate().is_err());
    }
}
