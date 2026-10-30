//! Release-channel version comparison.
//!
//! Stable releases are plain semver (`0.2.0`). Nightlies carry a build suffix
//! (`0.2.0+nightly.20261027.a1b2c3d`) so they sort after the stable release
//! they were cut from but against each other by date.
//!
//! This lives in the core crate rather than the app so it is covered by the
//! workspace test run — `app/src-tauri` is deliberately outside the workspace.

/// A parsed version: the semver triple plus an optional numeric build stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Date stamp from a `+nightly.<YYYYMMDD>` suffix; 0 for a stable build.
    /// Ordered *after* the triple, so 0.2.0 < 0.2.0+nightly.20261027.
    pub build: u64,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('v');
        let (core, build) = match s.split_once('+') {
            Some((c, b)) => (c, parse_build(b)),
            None => (s, 0),
        };
        // Ignore any pre-release tail; the build stamp carries the ordering.
        let core = core.split('-').next().unwrap_or(core);
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some(Self {
            major,
            minor,
            patch,
            build,
        })
    }
}

/// Pull the first long run of digits out of a build suffix, which is the date.
fn parse_build(b: &str) -> u64 {
    b.split(|c: char| !c.is_ascii_digit())
        .find(|p| p.len() >= 6)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0)
}

/// Is `candidate` strictly newer than `current`?
///
/// Unparseable input is never treated as newer: a malformed manifest must not
/// nag the user into installing something.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_bump_is_newer() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.1"));
    }

    #[test]
    fn same_version_is_not_newer() {
        assert!(!is_newer("0.1.0", "0.1.0"));
    }

    #[test]
    fn nightlies_order_by_date() {
        assert!(is_newer(
            "0.1.0+nightly.20261028.abc",
            "0.1.0+nightly.20261027.def"
        ));
        assert!(!is_newer(
            "0.1.0+nightly.20261027.abc",
            "0.1.0+nightly.20261028.def"
        ));
    }

    #[test]
    fn a_nightly_is_newer_than_the_stable_it_was_cut_from() {
        assert!(is_newer("0.1.0+nightly.20261027.abc", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0+nightly.20261027.abc"));
    }

    #[test]
    fn a_later_stable_beats_an_older_nightly() {
        assert!(is_newer("0.2.0", "0.1.0+nightly.20261027.abc"));
    }

    #[test]
    fn leading_v_is_tolerated() {
        assert!(is_newer("v0.2.0", "0.1.0"));
    }

    #[test]
    fn garbage_never_prompts_an_update() {
        assert!(!is_newer("", "0.1.0"));
        assert!(!is_newer("not-a-version", "0.1.0"));
        assert!(!is_newer("0.1.1", "also-not-a-version"));
    }

    #[test]
    fn major_dominates_build_stamp() {
        assert!(is_newer("1.0.0", "0.9.9+nightly.29991231.abc"));
    }
}
