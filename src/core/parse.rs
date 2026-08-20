//! Shared scalar-value parsers. One token set for boolean spellings so a value
//! can never mean different things across the write-coerce edge, the SQL and
//! in-memory filter evaluators, and the auth `_locked`/`_verified` reader — the
//! divergence that let a `_locked = "on"` read as *not locked* (fail-open).

/// Parse a boolean spelling, tri-state: `Some(true)` for `1`/`true`/`yes`/`on`,
/// `Some(false)` for `0`/`false`/`no`/`off`, `None` for anything else. Trimmed
/// and case-insensitive.
#[must_use]
pub fn parse_bool(s: &str) -> Option<bool> {
    let s = s.trim();
    if ["1", "true", "yes", "on"]
        .iter()
        .any(|t| s.eq_ignore_ascii_case(t))
    {
        return Some(true);
    }
    if ["0", "false", "no", "off"]
        .iter()
        .any(|t| s.eq_ignore_ascii_case(t))
    {
        return Some(false);
    }
    None
}

/// Truthy test: `true` only for a recognized true spelling (`1`/`true`/`yes`/
/// `on`, trimmed + case-insensitive); every other value — unrecognized OR a
/// false spelling — is `false`. The canonical checkbox/flag truthiness, used at
/// the write-coerce edge and by the auth flag reader (where the *safe* direction
/// for `_locked` is "any truthy spelling counts as locked").
#[must_use]
pub fn parse_truthy(s: &str) -> bool {
    parse_bool(s) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bool_tristate_case_insensitive() {
        for t in [
            "1", "true", "TRUE", "True", "yes", "YES", "on", "On", " on ",
        ] {
            assert_eq!(parse_bool(t), Some(true), "{t:?}");
        }
        for f in ["0", "false", "FALSE", "no", "NO", "off", "Off"] {
            assert_eq!(parse_bool(f), Some(false), "{f:?}");
        }
        for u in ["", "maybe", "2", "yep"] {
            assert_eq!(parse_bool(u), None, "{u:?}");
        }
    }

    #[test]
    fn parse_truthy_recognizes_on_and_case() {
        // The fail-open regression: `on`/`On` must be truthy (was missing from
        // the auth flag reader, which also skipped case-folding).
        assert!(parse_truthy("on"));
        assert!(parse_truthy("On"));
        assert!(parse_truthy("TRUE"));
        // Unrecognized / false spellings are not truthy.
        assert!(!parse_truthy("off"));
        assert!(!parse_truthy(""));
        assert!(!parse_truthy("nope"));
    }
}
