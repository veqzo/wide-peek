//! HTTPS GET through WinHTTP. No TLS crate: Windows does TLS and the proxy.
//!
//! The requests send only the generic User-Agent `widepeek/<version>` and the
//! headers below. They carry no personal data and go only to GitHub hosts.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2,
    WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_OPTION_SECURE_PROTOCOLS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

const USER_AGENT: &str = concat!("widepeek/", env!("CARGO_PKG_VERSION"));
const HTTPS_PORT: u16 = 443;
/// Timeouts in milliseconds: name resolution, connect, send, receive.
const TIMEOUTS_MS: (i32, i32, i32, i32) = (10_000, 10_000, 15_000, 30_000);
/// Limit for one `get`, with all redirects and the body.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_REDIRECTS: usize = 5;
const LOCATION_MAX_CHARS: usize = 8192;
/// The GitHub API, the release page and the hosts that serve the assets.
const ALLOWED_HOSTS: [&str; 4] = [
    "api.github.com",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// An owned WinHTTP handle.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> anyhow::Result<Self> {
        if raw.is_null() {
            return Err(std::io::Error::last_os_error()).context(format!("cannot {what}"));
        }
        Ok(Self(raw))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// Host and path of an `https://` URL. Other schemes, ports and user info are
/// refused.
#[derive(Debug, PartialEq, Eq)]
pub struct HttpsUrl<'a> {
    pub host: &'a str,
    pub path: &'a str,
}

pub fn parse_https_url(url: &str) -> anyhow::Result<HttpsUrl<'_>> {
    let rest = url
        .strip_prefix("https://")
        .with_context(|| format!("not an https URL: {url}"))?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    if !host_ok {
        bail!("not a plain https host: {url}");
    }
    if path.bytes().any(|b| b.is_ascii_control() || b == b' ') {
        bail!("the URL path has invalid characters: {url}");
    }
    let path = if path.is_empty() { "/" } else { path };
    Ok(HttpsUrl { host, path })
}

/// GETs `url` and returns the body. Fails on a status other than 200, a
/// body larger than `max_bytes` or after [`TOTAL_TIMEOUT`]. Redirects are
/// followed here, not by WinHTTP, so each hop must be an https URL on a
/// GitHub host.
pub fn get(url: &str, accept: &str, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let deadline = Instant::now() + TOTAL_TIMEOUT;
    let session = open_session()?;
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        check_deadline(deadline)?;
        let target = parse_allowed_url(&url)?;
        let (_connection, request) = send(&session, &target, accept)?;
        match status_code(&request)? {
            200 => return read_body(&request, max_bytes, deadline),
            301 | 302 | 303 | 307 | 308 => {
                let next = location(&request)?;
                // Only the host: the query of a download URL holds a token.
                let next_host = parse_https_url(&next).map_or("?", |u| u.host);
                info!("{} redirects to {next_host}", target.host);
                url = next;
            }
            status => bail!("{} answered with HTTP status {status}", target.host),
        }
    }
    bail!("too many redirects for {url}")
}

/// Parses `url` and checks that its host is on [`ALLOWED_HOSTS`].
fn parse_allowed_url(url: &str) -> anyhow::Result<HttpsUrl<'_>> {
    let target = parse_https_url(url)?;
    if !ALLOWED_HOSTS
        .iter()
        .any(|h| h.eq_ignore_ascii_case(target.host))
    {
        bail!("{} is not a GitHub download host", target.host);
    }
    Ok(target)
}

fn check_deadline(deadline: Instant) -> anyhow::Result<()> {
    if Instant::now() > deadline {
        bail!(
            "the download took longer than {} minutes",
            TOTAL_TIMEOUT.as_secs() / 60
        );
    }
    Ok(())
}

fn open_session() -> anyhow::Result<Handle> {
    // SAFETY: the strings live until the call returns.
    let session = Handle::new(
        unsafe {
            WinHttpOpen(
                &HSTRING::from(USER_AGENT),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        },
        "open a WinHTTP session",
    )?;
    let (resolve, connect, send, receive) = TIMEOUTS_MS;
    // SAFETY: `session` is a valid session handle.
    unsafe { WinHttpSetTimeouts(session.0, resolve, connect, send, receive) }
        .context("cannot set the HTTP timeouts")?;
    let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes();
    // SAFETY: the buffer is a u32, as the option expects.
    unsafe {
        WinHttpSetOption(
            Some(session.0),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&never),
        )
    }
    .context("cannot turn off the automatic HTTP redirects")?;
    let protocols = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
    // TLS 1.3 needs Windows 11. Without it, the default protocols stay.
    // SAFETY: the buffer is a u32, as the option expects.
    if unsafe {
        WinHttpSetOption(
            Some(session.0),
            WINHTTP_OPTION_SECURE_PROTOCOLS,
            Some(&protocols.to_ne_bytes()),
        )
    }
    .is_err()
    {
        let tls12 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2.to_ne_bytes();
        // SAFETY: as above.
        let _ = unsafe {
            WinHttpSetOption(
                Some(session.0),
                WINHTTP_OPTION_SECURE_PROTOCOLS,
                Some(&tls12),
            )
        };
    }
    Ok(session)
}

/// Sends one GET and waits for the response headers. Returns the connection
/// and the request; the request needs the connection.
fn send(session: &Handle, target: &HttpsUrl, accept: &str) -> anyhow::Result<(Handle, Handle)> {
    // SAFETY: the host string lives until the call returns.
    let connection = Handle::new(
        unsafe { WinHttpConnect(session.0, &HSTRING::from(target.host), HTTPS_PORT, 0) },
        "connect to the update server",
    )?;
    // SAFETY: the strings live until the call returns; no accept-type list.
    let request = Handle::new(
        unsafe {
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(target.path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        },
        "create the HTTP request",
    )?;
    let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
    // SAFETY: the header buffer lives until the call returns; no body.
    unsafe { WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0) }
        .with_context(|| format!("cannot reach {}", target.host))?;
    // SAFETY: `request` is a sent request handle.
    unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
        .with_context(|| format!("no answer from {}", target.host))?;
    Ok((connection, request))
}

/// The `Location` header of a redirect.
fn location(request: &Handle) -> anyhow::Result<String> {
    let mut buf = vec![0u16; LOCATION_MAX_CHARS];
    let mut len = (buf.len() * size_of::<u16>()) as u32;
    // SAFETY: the buffer has the size in bytes that we pass.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buf.as_mut_ptr().cast()),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .context("the redirect has no valid Location header")?;
    let chars = (len as usize / size_of::<u16>()).min(buf.len());
    String::from_utf16(&buf[..chars]).context("the Location header is not valid text")
}

fn status_code(request: &Handle) -> anyhow::Result<u32> {
    let mut status = 0u32;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: the buffer is a u32 with the size we pass.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&raw mut status).cast()),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .context("cannot read the HTTP status")?;
    Ok(status)
}

fn read_body(request: &Handle, max_bytes: usize, deadline: Instant) -> anyhow::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        check_deadline(deadline)?;
        let mut read = 0u32;
        // SAFETY: `chunk` has the length we pass.
        unsafe {
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast(),
                chunk.len() as u32,
                &mut read,
            )
        }
        .context("the download stopped")?;
        if read == 0 {
            return Ok(body);
        }
        if body.len() + read as usize > max_bytes {
            bail!("the download is larger than {max_bytes} bytes");
        }
        body.extend_from_slice(&chunk[..read as usize]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_https_urls() {
        assert_eq!(
            parse_https_url("https://api.github.com/repos/a/b/releases/latest").unwrap(),
            HttpsUrl {
                host: "api.github.com",
                path: "/repos/a/b/releases/latest"
            }
        );
        assert_eq!(parse_https_url("https://github.com").unwrap().path, "/");
    }

    #[test]
    fn refuses_other_urls() {
        assert!(parse_https_url("http://github.com/x").is_err());
        assert!(parse_https_url("https://user@github.com/x").is_err());
        assert!(parse_https_url("https://github.com:8443/x").is_err());
        assert!(parse_https_url("https:///x").is_err());
        assert!(parse_https_url("https://github.com/a b").is_err());
    }

    #[test]
    #[ignore = "needs the network"]
    fn follows_the_release_asset_redirect() {
        let url = concat!(
            env!("CARGO_PKG_REPOSITORY"),
            "/releases/download/v0.1.0/widepeek.exe.sha256"
        );
        let body = get(url, "application/octet-stream", 4096).unwrap();
        assert!(String::from_utf8(body).unwrap().contains("widepeek.exe"));
    }

    #[test]
    fn allows_only_github_hosts() {
        assert!(parse_allowed_url("https://api.github.com/repos/a/b").is_ok());
        assert!(parse_allowed_url("https://GitHub.com/a/b/releases").is_ok());
        assert!(parse_allowed_url("https://release-assets.githubusercontent.com/x").is_ok());
        assert!(parse_allowed_url("https://objects.githubusercontent.com/x").is_ok());
        assert!(parse_allowed_url("https://evil.example/x").is_err());
        assert!(parse_allowed_url("https://github.com.evil.example/x").is_err());
        assert!(parse_allowed_url("https://raw.githubusercontent.com/x").is_err());
        assert!(parse_allowed_url("http://github.com/x").is_err());
    }
}
