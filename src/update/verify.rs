//! Checks for a downloaded exe: SHA-256 and the Authenticode signature.

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use anyhow::{Context, bail};
use windows::Win32::Foundation::{CERT_E_REVOCATION_FAILURE, HANDLE, HWND, TRUST_E_NOSIGNATURE};
use windows::Win32::Security::Cryptography::{
    BCRYPT_SHA256_ALG_HANDLE, BCryptHash, CERT_NAME_RDN_TYPE, CERT_X500_NAME_STR,
    CertGetNameStringW,
};
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, WTD_REVOKE_WHOLECHAIN,
    WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE, WTHelperGetProvCertFromChain,
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};
use windows::core::{HSTRING, PCWSTR};

use crate::wide::from_wide;

pub type Sha256 = [u8; 32];

pub fn sha256(data: &[u8]) -> anyhow::Result<Sha256> {
    let mut out = [0u8; 32];
    // SAFETY: the SHA-256 pseudo handle needs no setup; the output has 32 bytes.
    unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) }
        .ok()
        .context("cannot compute the SHA-256 hash")?;
    Ok(out)
}

pub fn to_hex(hash: &Sha256) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses 64 hex digits (either case).
pub fn parse_hex(text: &str) -> anyhow::Result<Sha256> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("`{text}` is not a SHA-256 hash");
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

/// Parses a `sha256sum` line, for example `<hash>  widepeek.exe`. The file
/// name must be `file_name` (a leading `*` for binary mode is allowed).
pub fn parse_sha256_file(text: &str, file_name: &str) -> anyhow::Result<Sha256> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .context("the SHA-256 file is empty")?;
    let mut parts = line.split_whitespace();
    let hash = parts.next().context("the SHA-256 file is empty")?;
    let name = parts.next().map(|n| n.trim_start_matches('*'));
    if name != Some(file_name) || parts.next().is_some() {
        bail!("the SHA-256 file is not for {file_name}");
    }
    parse_hex(hash)
}

/// Result of the Authenticode check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    Unsigned,
    /// A valid signature that chains to a trusted root. Holds the full
    /// subject name of the signing certificate.
    Valid {
        subject: String,
    },
}

/// Checks the Authenticode signature of an open file. `path` must be the
/// path of `file`. A signature that is present but not valid is an error.
pub fn signature(file: &File, path: &Path) -> anyhow::Result<Signature> {
    let path_w = HSTRING::from(path.as_os_str());
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path_w.as_ptr()),
        hFile: HANDLE(file.as_raw_handle()),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` and `file_info` live until the close call below.
    let status = unsafe { WinVerifyTrust(HWND(-1isize as _), &mut action, (&raw mut data).cast()) };
    let result = match status {
        0 => signer_subject(data.hWVTStateData).map(|subject| Signature::Valid { subject }),
        s if s == TRUST_E_NOSIGNATURE.0 => Ok(Signature::Unsigned),
        s if s == CERT_E_REVOCATION_FAILURE.0 => Err(anyhow::anyhow!(
            "Windows cannot check if the certificate of {} is revoked. \
             Check the internet connection.",
            path.display()
        )),
        s => Err(anyhow::anyhow!(
            "the signature of {} is not valid (0x{:08x})",
            path.display(),
            s as u32
        )),
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: closes the state that the verify call opened.
    let _ = unsafe { WinVerifyTrust(HWND(-1isize as _), &mut action, (&raw mut data).cast()) };
    result
}

/// Subject of the first signer's certificate in the verify state.
fn signer_subject(state: HANDLE) -> anyhow::Result<String> {
    // SAFETY: `state` is open until the caller closes it; each pointer is
    // checked before use and points into that state.
    unsafe {
        let provider = WTHelperProvDataFromStateData(state);
        if provider.is_null() {
            bail!("cannot read the signature data");
        }
        let signer = WTHelperGetProvSignerFromChain(provider, 0, false, 0);
        if signer.is_null() {
            bail!("the signature has no signer");
        }
        let cert = WTHelperGetProvCertFromChain(signer, 0);
        if cert.is_null() || (*cert).pCert.is_null() {
            bail!("the signature has no signer certificate");
        }
        // The full subject, for example `CN=..., O=..., C=US`. A display
        // name alone could come from O or OU.
        let string_type = CERT_X500_NAME_STR.0;
        let para = Some((&raw const string_type).cast::<c_void>());
        let len = CertGetNameStringW((*cert).pCert, CERT_NAME_RDN_TYPE, 0, para, None);
        let mut buf = vec![0u16; len as usize];
        let len = CertGetNameStringW((*cert).pCert, CERT_NAME_RDN_TYPE, 0, para, Some(&mut buf));
        // `len` counts the closing NUL.
        let name = from_wide(&buf[..(len as usize).min(buf.len())]);
        if name.is_empty() {
            bail!("the signer certificate has no subject");
        }
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn hashes_with_sha256() {
        assert_eq!(to_hex(&sha256(b"").unwrap()), EMPTY_SHA256);
    }

    #[test]
    fn parses_sha256sum_lines() {
        let expected = parse_hex(EMPTY_SHA256).unwrap();
        let line = format!("{EMPTY_SHA256}  widepeek.exe\n");
        assert_eq!(parse_sha256_file(&line, "widepeek.exe").unwrap(), expected);
        let binary = format!("{} *widepeek.exe", EMPTY_SHA256.to_uppercase());
        assert_eq!(
            parse_sha256_file(&binary, "widepeek.exe").unwrap(),
            expected
        );
    }

    #[test]
    fn refuses_bad_sha256_files() {
        assert!(parse_sha256_file("", "widepeek.exe").is_err());
        assert!(parse_sha256_file(EMPTY_SHA256, "widepeek.exe").is_err());
        let other = format!("{EMPTY_SHA256}  other.exe");
        assert!(parse_sha256_file(&other, "widepeek.exe").is_err());
        assert!(parse_sha256_file("abc  widepeek.exe", "widepeek.exe").is_err());
        assert!(parse_hex(&"g".repeat(64)).is_err());
    }
}
