//! Version-tag utilities — current binary version, tag normalization,
//! and semver-based comparison for "is this newer?".

use semver::Version;

/// The current binary's version string, as a `vX.Y.Z[-prerelease]` tag.
pub(super) fn current_version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// Accept both `v0.1.0-alpha.5` and `0.1.0-alpha.5` on input; emit with `v`.
pub(super) fn normalize_tag(input: &str) -> String {
    if input.starts_with('v') {
        input.to_string()
    } else {
        format!("v{input}")
    }
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
        assert_eq!(normalize_tag("0.1.0-alpha.5"), "v0.1.0-alpha.5");
        assert_eq!(normalize_tag("v0.1.0-alpha.5"), "v0.1.0-alpha.5");
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
