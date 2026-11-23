//! User configuration in `%AppData%\widepeek\config.toml`. Setup writes it.

use std::path::PathBuf;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::customres::{AMD_SCALING_MODE_MAX, AmdScaling, ScalingBefore};
use crate::display::DisplayMode;
use crate::paths;

pub const DEFAULT_START_TIMEOUT_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// GDI name of the display, for example `\\.\DISPLAY1`.
    pub display_name: String,
    pub stretch_width: u32,
    pub stretch_height: u32,
    pub refresh_hz: u32,
    pub native_width: u32,
    pub native_height: u32,
    pub native_hz: u32,
    pub native_bpp: u32,
    /// Disables all monitor devices while the game runs (true stretched).
    /// Older configs also have `monitor_instance_id`; widepeek ignores it.
    #[serde(default = "default_true")]
    pub toggle_monitor: bool,
    /// Overrides the Riot Client path from `RiotClientInstalls.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub riot_client_path: Option<PathBuf>,
    #[serde(default = "default_start_timeout")]
    pub start_timeout_secs: u64,
    #[serde(default)]
    pub tray_autostart: bool,
    /// Checks GitHub for a new release at most once per day. Off by default:
    /// widepeek sends nothing over the network unless the user turns it on.
    #[serde(default)]
    pub check_updates: bool,
    /// The NVIDIA scaling (`NV_SCALING`) before setup set it to Full-screen
    /// on the GPU. `Some` means that widepeek manages the scaling: a session
    /// sets it again if the driver lost it, and uninstall restores this value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_scaling_before: Option<i32>,
    /// The AMD scaling before setup set GPU Scaling on with Full panel.
    /// Same meaning as `gpu_scaling_before`. At most one of both is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amd_scaling_before: Option<AmdScaling>,
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

    /// The scaling from before setup, if widepeek manages the scaling.
    pub fn scaling_before(&self) -> Option<ScalingBefore> {
        self.gpu_scaling_before
            .map(ScalingBefore::Nvidia)
            .or(self.amd_scaling_before.map(ScalingBefore::Amd))
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
        if !(10..=3600).contains(&self.start_timeout_secs) {
            bail!("start_timeout_secs must be between 10 and 3600");
        }
        // The values of `NV_SCALING` in the NVAPI SDK.
        if let Some(value) = self.gpu_scaling_before
            && !matches!(value, 0..=8 | 255)
        {
            bail!("gpu_scaling_before ({value}) is not an NVIDIA scaling value");
        }
        if let Some(amd) = self.amd_scaling_before
            && !(0..=AMD_SCALING_MODE_MAX).contains(&amd.mode)
        {
            bail!(
                "amd_scaling_before.mode ({}) is not an AMD scaling mode",
                amd.mode
            );
        }
        if self.gpu_scaling_before.is_some() && self.amd_scaling_before.is_some() {
            bail!("set only one of gpu_scaling_before and amd_scaling_before");
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
        check_updates: false,
        gpu_scaling_before: None,
        amd_scaling_before: None,
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
    fn update_checks_are_off_in_old_configs() {
        let mut config = sample();
        config.check_updates = true;
        let text: String = config
            .to_toml()
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with("check_updates"))
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(!Config::from_toml(&text).unwrap().check_updates);
    }

    #[test]
    fn round_trips_the_gpu_scaling() {
        let mut config = sample();
        config.gpu_scaling_before = Some(6);
        let text = config.to_toml().unwrap();
        assert!(text.contains("gpu_scaling_before = 6"));
        assert_eq!(Config::from_toml(&text).unwrap(), config);
    }

    #[test]
    fn old_configs_do_not_manage_the_gpu_scaling() {
        let text = sample().to_toml().unwrap();
        assert!(!text.contains("gpu_scaling_before"));
        assert_eq!(Config::from_toml(&text).unwrap().gpu_scaling_before, None);
    }

    #[test]
    fn rejects_unknown_gpu_scaling() {
        let mut config = sample();
        config.gpu_scaling_before = Some(42);
        assert!(config.validate().is_err());
    }

    fn amd_scaling() -> AmdScaling {
        AmdScaling {
            enabled: false,
            mode: 0,
            integer: true,
        }
    }

    #[test]
    fn round_trips_the_amd_scaling() {
        let mut config = sample();
        config.amd_scaling_before = Some(amd_scaling());
        let text = config.to_toml().unwrap();
        assert!(text.contains("amd_scaling_before"));
        let read = Config::from_toml(&text).unwrap();
        assert_eq!(read, config);
        assert_eq!(
            read.scaling_before(),
            Some(ScalingBefore::Amd(amd_scaling()))
        );
    }

    #[test]
    fn merges_the_scaling_by_vendor() {
        let mut config = sample();
        assert_eq!(config.scaling_before(), None);
        config.gpu_scaling_before = Some(6);
        assert_eq!(config.scaling_before(), Some(ScalingBefore::Nvidia(6)));
    }

    #[test]
    fn rejects_unknown_amd_scaling_mode() {
        let mut config = sample();
        config.amd_scaling_before = Some(AmdScaling {
            mode: 3,
            ..amd_scaling()
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_scaling_of_two_vendors() {
        let mut config = sample();
        config.gpu_scaling_before = Some(6);
        config.amd_scaling_before = Some(amd_scaling());
        assert!(config.validate().is_err());
    }

    #[test]
    fn ignores_old_monitor_id() {
        let text = format!(
            "monitor_instance_id = 'DISPLAY\\X'\n{}",
            sample().to_toml().unwrap()
        );
        assert_eq!(Config::from_toml(&text).unwrap(), sample());
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
