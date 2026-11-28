//! Riot Client lookup and the Valorant process names.

use std::path::{Path, PathBuf};

use anyhow::bail;
use serde_json::Value;

use crate::paths;

/// The game process. Its end ends the stretch session.
pub const GAME_PROCESS: &str = "VALORANT-Win64-Shipping.exe";
/// The Valorant bootstrapper. It starts shortly before the game process.
pub const BOOTSTRAP_PROCESS: &str = "VALORANT.exe";
pub const LAUNCH_ARGS: [&str; 2] = ["--launch-product=valorant", "--launch-patchline=live"];

pub const CLIENT_EXE: &str = "RiotClientServices.exe";
const DEFAULT_CLIENT: &str = r"C:\Riot Games\Riot Client\RiotClientServices.exe";
const DEFAULT_GAME_DIR: &str = r"C:\Riot Games\VALORANT\live";

/// Finds `RiotClientServices.exe`. Order: the path from the config (if the
/// file exists), `RiotClientInstalls.json`, then the default install path.
pub fn client_path(configured: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(path) = configured {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        warn!(
            "riot_client_path {} does not exist, searching",
            path.display()
        );
    }
    let from_json = installs_json()
        .map(|json| candidates(&json))
        .unwrap_or_default();
    let default = PathBuf::from(DEFAULT_CLIENT);
    if let Some(path) = from_json
        .into_iter()
        .chain([default])
        .find(|p| is_client(p) && p.is_file())
    {
        return Ok(path);
    }
    bail!(
        "cannot find {CLIENT_EXE}. Set riot_client_path in {}",
        paths::config_file().map_or_else(|_| "config.toml".to_owned(), |p| p.display().to_string())
    )
}

/// The processes that run while the game starts: the Riot Client runs
/// first, then `VALORANT.exe`, then the game. If `riot_client_path` points
/// to an exe with another name, that name also counts.
pub fn start_processes(client: &Path) -> Vec<String> {
    let mut names = vec![CLIENT_EXE.to_owned(), BOOTSTRAP_PROCESS.to_owned()];
    let configured = client.file_name().and_then(|n| n.to_str());
    if let Some(name) =
        configured.filter(|name| !names.iter().any(|n| n.eq_ignore_ascii_case(name)))
    {
        names.push(name.to_owned());
    }
    names
}

/// `VALORANT.exe` of the live install, for the shortcut icon. The keys of
/// `associated_client` are the game folders.
pub fn valorant_exe() -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = installs_json()
        .as_ref()
        .and_then(|json| json.get("associated_client")?.as_object())
        .map(|map| {
            map.keys()
                .map(|k| PathBuf::from(k.replace('/', "\\")))
                .collect()
        })
        .unwrap_or_default();
    dirs.into_iter()
        .chain([PathBuf::from(DEFAULT_GAME_DIR)])
        .map(|dir| dir.join(BOOTSTRAP_PROCESS))
        .find(|path| path.is_file())
}

fn installs_json() -> Option<Value> {
    let file = paths::program_data()
        .ok()?
        .join("Riot Games")
        .join("RiotClientInstalls.json");
    let bytes = std::fs::read(file).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Paths from `RiotClientInstalls.json`: `rc_default`, `rc_live`, then the
/// values of `associated_client`.
fn candidates(json: &Value) -> Vec<PathBuf> {
    let single = ["rc_default", "rc_live"]
        .into_iter()
        .filter_map(|key| json.get(key)?.as_str());
    let associated = json
        .get("associated_client")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|map| map.values().filter_map(Value::as_str));
    single
        .chain(associated)
        .map(|s| PathBuf::from(s.replace('/', "\\")))
        .collect()
}

fn is_client(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(CLIENT_EXE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_processes_include_a_configured_client_name() {
        let default = start_processes(Path::new(DEFAULT_CLIENT));
        assert_eq!(default, [CLIENT_EXE, BOOTSTRAP_PROCESS]);
        let lower = start_processes(Path::new(r"D:\Riot\riotclientservices.exe"));
        assert_eq!(lower, [CLIENT_EXE, BOOTSTRAP_PROCESS]);
        let other = start_processes(Path::new(r"D:\Tools\RiotLauncher.exe"));
        assert_eq!(other, [CLIENT_EXE, BOOTSTRAP_PROCESS, "RiotLauncher.exe"]);
    }

    #[test]
    fn reads_candidates_in_order() {
        let json: Value = serde_json::from_str(
            r#"{
                "associated_client": {"C:/Riot Games/VALORANT/live/": "D:/Riot/RiotClientServices.exe"},
                "rc_default": "C:/Riot Games/Riot Client/RiotClientServices.exe",
                "rc_live": "E:/RC/RiotClientServices.exe"
            }"#,
        )
        .unwrap();
        assert_eq!(
            candidates(&json),
            vec![
                PathBuf::from(r"C:\Riot Games\Riot Client\RiotClientServices.exe"),
                PathBuf::from(r"E:\RC\RiotClientServices.exe"),
                PathBuf::from(r"D:\Riot\RiotClientServices.exe"),
            ]
        );
    }

    #[test]
    fn accepts_only_the_client_exe() {
        assert!(is_client(Path::new(r"C:\x\riotclientservices.EXE")));
        assert!(!is_client(Path::new(r"C:\x\evil.exe")));
    }
}
