//! `%LocalAppData%\widepeek\update.json`: the time of the last update check.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::paths;

/// At most one automatic check per this interval.
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stamp {
    last_check_unix: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// True if the last check is older than `INTERVAL`. A missing or broken
/// file, or a last check in the future (clock change), means "due".
pub fn is_due() -> bool {
    let last = paths::update_stamp_file()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Stamp>(&bytes).ok())
        .map_or(0, |s| s.last_check_unix);
    due(last, now_unix())
}

fn due(last: u64, now: u64) -> bool {
    last > now || now - last >= INTERVAL.as_secs()
}

/// Saves "checked now". Errors only go to the log.
pub fn mark_checked() {
    let stamp = Stamp {
        last_check_unix: now_unix(),
    };
    let result = paths::update_stamp_file().and_then(|path| {
        let json = serde_json::to_vec(&stamp)?;
        paths::write_atomic(&path, &json)
    });
    if let Err(err) = result {
        warn!("cannot save the time of the update check: {err:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_due_once_per_day() {
        let day = INTERVAL.as_secs();
        assert!(due(0, 1_000_000));
        assert!(!due(1_000_000, 1_000_000 + day - 1));
        assert!(due(1_000_000, 1_000_000 + day));
        assert!(due(2_000_000, 1_000_000), "a clock change makes it due");
    }
}
