//! The latest GitHub release: parse the API answer and compare versions.

use std::fmt;

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::paths::EXE_NAME;

/// A version `X.Y.Z` with numbers only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Parses `X.Y.Z` or `vX.Y.Z`. Pre-release and build suffixes are refused.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let bare = text.strip_prefix('v').unwrap_or(text);
        let parts: Vec<&str> = bare.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            bail!("`{text}` is not a version X.Y.Z");
        };
        let number = |part: &str| -> anyhow::Result<u32> {
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                bail!("`{text}` is not a version X.Y.Z");
            }
            part.parse()
                .with_context(|| format!("`{text}` has a too large number"))
        };
        Ok(Self(number(major)?, number(minor)?, number(patch)?))
    }

    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is X.Y.Z")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A release with the two files that the updater needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub page_url: String,
    pub exe_url: String,
    pub sha256_url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// `https://github.com/<owner>/<repo>` from the `repository` field of
/// Cargo.toml, without a trailing slash.
pub fn repository_url() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_end_matches('/')
}

fn api_releases_url() -> anyhow::Result<String> {
    let repo = repository_url()
        .strip_prefix("https://github.com/")
        .context("the repository in Cargo.toml is not a github.com URL")?;
    Ok(format!("https://api.github.com/repos/{repo}/releases"))
}

/// The GitHub API URL of the latest release.
pub fn latest_api_url() -> anyhow::Result<String> {
    Ok(format!("{}/latest", api_releases_url()?))
}

/// The GitHub API URL of the release with the tag `v<version>`.
pub fn tag_api_url(version: Version) -> anyhow::Result<String> {
    Ok(format!("{}/tags/v{version}", api_releases_url()?))
}

/// Parses the `releases/latest` answer. All URLs must point into the
/// repository of this crate.
pub fn parse(json: &[u8], repository: &str) -> anyhow::Result<Release> {
    let api: ApiRelease =
        serde_json::from_slice(json).context("the release answer is not valid JSON")?;
    if api.draft || api.prerelease {
        bail!("the latest release is a draft or a pre-release");
    }
    let version = Version::parse(&api.tag_name)?;
    let page_prefix = format!("{repository}/releases/");
    if !api.html_url.starts_with(&page_prefix) {
        bail!("the release page is not in {repository}");
    }
    let download_prefix = format!("{repository}/releases/download/{}/", api.tag_name);
    let asset = |name: &str| -> anyhow::Result<String> {
        let found = api
            .assets
            .iter()
            .find(|a| a.name == name)
            .with_context(|| format!("release {} has no file {name}", api.tag_name))?;
        if found.browser_download_url != format!("{download_prefix}{name}") {
            bail!("the download URL of {name} is not in {repository}");
        }
        Ok(found.browser_download_url.clone())
    };
    Ok(Release {
        version,
        page_url: api.html_url.clone(),
        exe_url: asset(EXE_NAME)?,
        sha256_url: asset(&format!("{EXE_NAME}.sha256"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPO: &str = "https://github.com/owner/repo";

    fn json(tag: &str, exe_url: &str) -> String {
        format!(
            r#"{{
                "tag_name": "{tag}",
                "html_url": "{REPO}/releases/tag/{tag}",
                "draft": false,
                "prerelease": false,
                "assets": [
                    {{"name": "widepeek.exe", "browser_download_url": "{exe_url}", "size": 1}},
                    {{"name": "widepeek.exe.sha256",
                      "browser_download_url": "{REPO}/releases/download/{tag}/widepeek.exe.sha256"}}
                ]
            }}"#
        )
    }

    #[test]
    fn parses_versions() {
        assert_eq!(Version::parse("v1.2.3").unwrap(), Version(1, 2, 3));
        assert_eq!(Version::parse("0.10.0").unwrap(), Version(0, 10, 0));
        for bad in [
            "1.2",
            "v1.2.3-rc1",
            "1.2.3.4",
            "1.x.3",
            "",
            "v",
            "1..3",
            "+1.2.3",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version(1, 2, 3).to_string(), "1.2.3");
    }

    #[test]
    fn parses_a_release() {
        let url = format!("{REPO}/releases/download/v0.2.0/widepeek.exe");
        let release = parse(json("v0.2.0", &url).as_bytes(), REPO).unwrap();
        assert_eq!(release.version, Version(0, 2, 0));
        assert_eq!(release.exe_url, url);
        assert!(release.sha256_url.ends_with("/v0.2.0/widepeek.exe.sha256"));
    }

    #[test]
    fn refuses_foreign_download_urls() {
        let bad = "https://example.com/widepeek.exe";
        assert!(parse(json("v0.2.0", bad).as_bytes(), REPO).is_err());
        let other_tag = format!("{REPO}/releases/download/v0.1.0/widepeek.exe");
        assert!(parse(json("v0.2.0", &other_tag).as_bytes(), REPO).is_err());
    }

    #[test]
    fn refuses_pre_releases_and_bad_tags() {
        let url = format!("{REPO}/releases/download/v0.2.0/widepeek.exe");
        let pre = json("v0.2.0", &url).replace(r#""prerelease": false"#, r#""prerelease": true"#);
        assert!(parse(pre.as_bytes(), REPO).is_err());
        assert!(parse(json("latest", &url).as_bytes(), REPO).is_err());
        assert!(parse(b"not json", REPO).is_err());
    }
}
