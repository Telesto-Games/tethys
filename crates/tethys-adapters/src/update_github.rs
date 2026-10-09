//! `UpdateSource` backed by GitHub Releases.
//!
//! Reads `GET /repos/{repo}/releases/latest` (which skips drafts and
//! pre-releases) and downloads the release's `tethys.exe` asset, checking it
//! against the SHA-256 GitHub records for every asset (`digest`).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tethys_core::ports::{PortResult, UpdateSource};
use tethys_core::update::{Release, Version};
use ureq::tls::{RootCerts, TlsConfig};

/// The release asset the updater installs.
pub const EXE_ASSET: &str = "tethys.exe";
/// Upper bound on the download, well above the exe's real size.
const MAX_EXE_BYTES: u64 = 512 * 1024 * 1024;

pub struct GitHubReleases {
    /// `owner/name`, e.g. `Telesto-Games/tethys`.
    repo: String,
    agent: ureq::Agent,
}

impl GitHubReleases {
    pub fn new(repo: impl Into<String>) -> Self {
        // Windows' certificate store, so it works behind TLS-inspecting proxies.
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::PlatformVerifier)
            .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .user_agent(concat!("tethys/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self {
            repo: repo.into(),
            agent,
        }
    }
}

impl UpdateSource for GitHubReleases {
    fn latest(&self) -> PortResult<Option<Release>> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", self.repo);
        let response = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .call();
        let mut response = match response {
            // No published release yet.
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            other => other.map_err(|e| format!("checking for updates: {e}"))?,
        };
        let latest: ApiRelease = response.body_mut().read_json()?;
        latest.into_release().map(Some)
    }

    fn download(&self, release: &Release, dest: &Path) -> PortResult<()> {
        let result = download_to(&self.agent, release, dest);
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }
}

fn download_to(agent: &ureq::Agent, release: &Release, dest: &Path) -> PortResult<()> {
    let mut response = agent
        .get(&release.exe_url)
        .call()
        .map_err(|e| format!("downloading {}: {e}", release.exe_url))?;
    let mut body = response
        .body_mut()
        .with_config()
        .limit(MAX_EXE_BYTES)
        .reader();
    let file = std::fs::File::create(dest)?;
    let actual = copy_hashed(&mut body, file)?;
    if actual != release.sha256 {
        return Err(format!(
            "the download is corrupt (SHA-256 {actual}, expected {})",
            release.sha256
        )
        .into());
    }
    Ok(())
}

/// Copies `from` into `to` and returns the SHA-256 of what was copied.
fn copy_hashed(from: &mut impl Read, mut to: impl Write) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        to.write_all(&buf[..n])?;
    }
    to.flush()?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The parts of GitHub's release JSON that Tethys uses.
#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    /// `sha256:<hex>`; GitHub computes it on upload.
    #[serde(default)]
    digest: Option<String>,
}

impl ApiRelease {
    fn into_release(self) -> PortResult<Release> {
        let version = Version::parse(&self.tag_name)
            .ok_or_else(|| format!("release tag {:?} isn't a version", self.tag_name))?;
        let asset = self
            .assets
            .into_iter()
            .find(|a| a.name == EXE_ASSET)
            .ok_or_else(|| format!("release {} has no {EXE_ASSET}", self.tag_name))?;
        let sha256 = asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| format!("{EXE_ASSET} in {} has no SHA-256", self.tag_name))?
            .to_ascii_lowercase();
        Ok(Release {
            version,
            notes: self.body.unwrap_or_default(),
            page_url: self.html_url,
            exe_url: asset.browser_download_url,
            sha256,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from a real `releases/latest` response.
    const LATEST: &str = r###"{
        "url": "https://api.github.com/repos/Telesto-Games/tethys/releases/1",
        "html_url": "https://github.com/Telesto-Games/tethys/releases/tag/v0.2.0",
        "tag_name": "v0.2.0",
        "name": "Tethys 0.2.0",
        "draft": false,
        "prerelease": false,
        "body": "## What's Changed\r\n* Things",
        "assets": [
            {
                "name": "tethys-0.2.0-windows-x64.zip",
                "browser_download_url": "https://github.com/Telesto-Games/tethys/releases/download/v0.2.0/tethys-0.2.0-windows-x64.zip",
                "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111"
            },
            {
                "name": "tethys.exe",
                "size": 41000000,
                "browser_download_url": "https://github.com/Telesto-Games/tethys/releases/download/v0.2.0/tethys.exe",
                "digest": "sha256:ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
            }
        ]
    }"###;

    #[test]
    fn maps_the_latest_release() {
        let api: ApiRelease = serde_json::from_str(LATEST).unwrap();
        let release = api.into_release().unwrap();
        assert_eq!(release.version, Version::parse("0.2.0").unwrap());
        assert_eq!(
            release.exe_url,
            "https://github.com/Telesto-Games/tethys/releases/download/v0.2.0/tethys.exe"
        );
        assert_eq!(
            release.sha256,
            "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        );
        assert!(release.page_url.ends_with("/tag/v0.2.0"));
        assert!(release.notes.starts_with("## What's Changed"));
    }

    #[test]
    fn a_release_without_the_exe_or_its_digest_is_an_error() {
        let mut api: ApiRelease = serde_json::from_str(LATEST).unwrap();
        api.assets.retain(|a| a.name != EXE_ASSET);
        assert!(
            api.into_release()
                .unwrap_err()
                .to_string()
                .contains("no tethys.exe")
        );

        let mut api: ApiRelease = serde_json::from_str(LATEST).unwrap();
        api.assets[1].digest = None;
        assert!(
            api.into_release()
                .unwrap_err()
                .to_string()
                .contains("SHA-256")
        );
    }

    #[test]
    fn hashes_what_it_copies() {
        let mut out = Vec::new();
        let hash = copy_hashed(&mut &b"abc"[..], &mut out).unwrap();
        assert_eq!(out, b"abc");
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Talks to GitHub: `cargo test -p tethys-adapters live -- --ignored`.
    #[test]
    #[ignore]
    fn reads_and_downloads_the_latest_release() {
        let source = GitHubReleases::new("Telesto-Games/tethys");
        let Some(release) = source.latest().unwrap() else {
            return;
        };
        eprintln!("latest: {} {}", release.version, release.exe_url);
        let dest = std::env::temp_dir().join(format!("tethys-live-{}.exe", std::process::id()));
        source.download(&release, &dest).unwrap();
        assert!(std::fs::metadata(&dest).unwrap().len() > 1_000_000);
        std::fs::remove_file(&dest).unwrap();

        // A wrong hash is rejected and leaves nothing behind.
        let tampered = Release {
            sha256: "0".repeat(64),
            ..release
        };
        let err = source.download(&tampered, &dest).unwrap_err();
        assert!(err.to_string().contains("corrupt"), "{err}");
        assert!(!dest.exists());
    }
}
