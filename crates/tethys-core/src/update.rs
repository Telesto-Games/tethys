//! Releases and versions, for the in-app updater.

use std::fmt;

/// A release version, `major.minor.patch`. A leading `v` and any pre-release
/// or build suffix (`-beta.1`, `+abc`) are ignored when parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let core = text.split(['-', '+']).next()?;
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        let version = Version {
            major: parts.next()??,
            minor: parts.next().unwrap_or(Some(0))?,
            patch: parts.next().unwrap_or(Some(0))?,
        };
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A published release that the updater can install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// Release notes, as written (Markdown).
    pub notes: String,
    /// The release's web page.
    pub page_url: String,
    /// Where to download the new `tethys.exe`.
    pub exe_url: String,
    /// Expected SHA-256 of the exe, lowercase hex.
    pub sha256: String,
}

/// Whether to offer `release` to someone running `current`. A version the
/// user chose to skip isn't offered again, unless they asked (`manual`).
pub fn should_offer(
    current: Version,
    release: &Release,
    skipped: Option<Version>,
    manual: bool,
) -> bool {
    release.version > current && (manual || skipped != Some(release.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn release(version: &str) -> Release {
        Release {
            version: v(version),
            notes: String::new(),
            page_url: String::new(),
            exe_url: String::new(),
            sha256: String::new(),
        }
    }

    #[test]
    fn parses_versions() {
        assert_eq!(
            v("v1.2.3"),
            Version {
                major: 1,
                minor: 2,
                patch: 3
            }
        );
        assert_eq!(v("0.10"), v("0.10.0"));
        assert_eq!(v("2.0.0-beta.1+abc"), v("2.0.0"));
        assert_eq!(v(" 0.1.0\n").to_string(), "0.1.0");
        for bad in ["", "v", "1.x", "1.2.3.4", "one"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn orders_numerically() {
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("1.0.0") > v("0.99.0"));
        assert!(v("0.1.1") > v("0.1.0"));
    }

    #[test]
    fn offers_only_newer_unskipped_releases() {
        let current = v("0.1.0");
        assert!(should_offer(current, &release("0.2.0"), None, false));
        assert!(!should_offer(current, &release("0.1.0"), None, false));
        assert!(!should_offer(current, &release("0.0.9"), None, true));
        // Skipped: not offered at startup, but a manual check still shows it.
        assert!(!should_offer(
            current,
            &release("0.2.0"),
            Some(v("0.2.0")),
            false
        ));
        assert!(should_offer(
            current,
            &release("0.2.0"),
            Some(v("0.2.0")),
            true
        ));
        // Skipping one version doesn't skip the next.
        assert!(should_offer(
            current,
            &release("0.3.0"),
            Some(v("0.2.0")),
            false
        ));
    }
}
