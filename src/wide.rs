//! UTF-16 helpers for Win32 calls.

/// Converts a NUL-terminated UTF-16 buffer into a `String`.
pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Converts a `&str` into a NUL-terminated UTF-16 buffer.
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_nul() {
        let buf: Vec<u16> = "abc\0def".encode_utf16().collect();
        assert_eq!(from_wide(&buf), "abc");
    }

    #[test]
    fn handles_missing_nul() {
        let buf: Vec<u16> = "abc".encode_utf16().collect();
        assert_eq!(from_wide(&buf), "abc");
    }

    #[test]
    fn round_trips() {
        assert_eq!(from_wide(&to_wide("DISPLAY1")), "DISPLAY1");
    }
}
