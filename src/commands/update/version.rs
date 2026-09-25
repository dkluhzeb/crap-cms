//! Version-tag utilities — current binary version, tag normalization,
//! and semver-based comparison for "is this newer?".

use anyhow::{Result, bail};
use semver::Version;

/// The current binary's version string, as a `vX.Y.Z[-prerelease]` tag.
pub(super) fn current_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Accept both `v0.1.0-alpha.5` and `0.1.0-alpha.5` on input; emit with `v`.
///
/// The tag becomes a directory name in the version store (`install`, `use`,
/// `uninstall` all build `versions/<tag>/` from it), so it must be a valid
/// semver version: that grammar admits only ASCII alphanumerics, `.`, `-`
/// and `+`, with no empty dot-separated identifier — no path separator and
/// no `..` component can survive it.
///
/// # Errors
///
/// Returns an error when the input is not a (optionally `v`-prefixed)
/// semver version.
pub(super) fn normalize_tag(input: &str) -> Result<String> {
    let tag = if input.starts_with('v') {
        input.to_string()
    } else {
        format!("v{input}")
    };

    validate_tag(&tag)?;

    Ok(tag)
}

/// Refuse anything that is not a `v`-prefixed semver tag.
///
/// # Errors
///
/// Returns an error naming the rejected input.
pub(super) fn validate_tag(tag: &str) -> Result<()> {
    let Some(bare) = tag.strip_prefix('v') else {
        bail!("invalid version {tag:?}: expected a tag like v0.1.0 or v0.1.0-alpha.5");
    };

    if Version::parse(bare).is_err() {
        bail!("invalid version {tag:?}: expected a tag like v0.1.0 or v0.1.0-alpha.5");
    }

    Ok(())
}

/// Parse a `vX.Y.Z-…` tag into a `semver::Version` (strips the leading `v`).
fn parse_tag(tag: &str) -> Option<Version> {
    let trimmed = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(trimmed).ok()
}

/// Is `candidate` a newer release than `current`?
pub(super) fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_tag(candidate), parse_tag(current)) {
        (Some(c), Some(n)) => c > n,
        _ => false, // conservative: if we can't parse, don't claim a newer one
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tag_adds_v_prefix() {
        assert_eq!(normalize_tag("0.1.0-alpha.5").unwrap(), "v0.1.0-alpha.5");
        assert_eq!(normalize_tag("v0.1.0-alpha.5").unwrap(), "v0.1.0-alpha.5");
    }

    #[test]
    fn normalize_tag_rejects_path_traversal() {
        for input in [
            "v0.1.0/../../..",
            "0.1.0/../../..",
            "../v0.1.0",
            "v0.1.0/..",
            "..",
            "v..",
            "v0.1.0\\..\\..",
            "/etc",
        ] {
            let err = normalize_tag(input).unwrap_err();
            assert!(
                format!("{err:#}").contains("invalid version"),
                "{input:?} must be rejected, got {err:#}"
            );
        }
    }

    #[test]
    fn normalize_tag_rejects_non_semver() {
        assert!(normalize_tag("nightly").is_err());
        assert!(normalize_tag("v1").is_err());
        assert!(normalize_tag("").is_err());
    }

    #[test]
    fn validate_tag_requires_v_prefix() {
        assert!(validate_tag("0.1.0").is_err());
        assert!(validate_tag("v0.1.0").is_ok());
        assert!(validate_tag("v1.0.0+build.7").is_ok());
    }

    #[test]
    fn is_newer_prerelease_order() {
        assert!(is_newer("v0.1.0-alpha.5", "v0.1.0-alpha.4"));
        assert!(!is_newer("v0.1.0-alpha.4", "v0.1.0-alpha.5"));
    }

    #[test]
    fn is_newer_stable_over_prerelease() {
        // semver: 1.0.0 > 1.0.0-alpha.5 (prereleases rank below)
        assert!(is_newer("v1.0.0", "v1.0.0-alpha.5"));
    }

    #[test]
    fn is_newer_same_version_is_false() {
        assert!(!is_newer("v0.1.0-alpha.5", "v0.1.0-alpha.5"));
    }

    #[test]
    fn is_newer_unparseable_is_false() {
        // Don't claim updates on junk input.
        assert!(!is_newer("nightly", "v0.1.0-alpha.5"));
        assert!(!is_newer("v0.1.0-alpha.5", "nightly"));
    }
}
