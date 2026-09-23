//! SQL `LIKE` pattern matching for the in-memory filter evaluator.

use regex::{Regex, escape};

/// SQL `LIKE` matching: `%` matches any run of characters, `_` one character,
/// and `\` escapes the next one. Agrees with the SQL backends so the in-memory
/// and SQL paths decide alike:
/// - literal characters are regex-escaped, so a `.`/`(`/`[` matches itself (an
///   over-match here would fail open);
/// - `%` spans line breaks, as in SQL;
/// - ASCII case is folded on both sides: `SQLite` `LIKE` is ASCII-case-insensitive
///   and Postgres uses `ILIKE`; ASCII-only folding tracks `SQLite` and stays at
///   most stricter than `ILIKE` for non-ASCII (fail-closed);
/// - a pattern ending in a lone `\` matches nothing (the SQL path rejects it).
pub(super) fn matches_like(value: &str, pattern: &str) -> bool {
    let Some(body) = like_to_regex(&pattern.to_ascii_lowercase()) else {
        return false;
    };

    Regex::new(&format!("(?s)^{body}$")).is_ok_and(|re| re.is_match(&value.to_ascii_lowercase()))
}

/// Translate a `LIKE` pattern to a regex body, or `None` when it ends in a
/// lone escape character.
fn like_to_regex(pattern: &str) -> Option<String> {
    let mut body = String::with_capacity(pattern.len() * 2);
    let mut chars = pattern.chars();

    while let Some(c) = chars.next() {
        match c {
            '\\' => body.push_str(&escape(&chars.next()?.to_string())),
            '%' => body.push_str(".*"),
            '_' => body.push('.'),
            other => body.push_str(&escape(&other.to_string())),
        }
    }

    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Like` must treat regex metacharacters in the pattern as literals (SQL
    /// semantics), not as regex — otherwise in-memory matching over-matches
    /// relative to SQL, a fail-open on the access-gating path.
    #[test]
    fn matches_like_treats_metacharacters_literally() {
        // `.` is a literal dot, not "any char": "a.c" matches, "axc" must not.
        assert!(matches_like("a.c", "a.c"));
        assert!(!matches_like("axc", "a.c"));

        // SQL wildcards still work: `%` = any run, `_` = any single char.
        assert!(matches_like("axc", "a%c"));
        assert!(matches_like("axc", "a_c"));

        // Regex group/alternation metachars are literal too.
        assert!(matches_like("a(b)c", "a(b)c"));
        assert!(!matches_like("ab", "a|b"));
    }

    /// `Like` is ASCII-case-insensitive on both sides, matching `SQLite` `LIKE` /
    /// Postgres `ILIKE`, so the in-memory and SQL paths agree.
    #[test]
    fn matches_like_is_ascii_case_insensitive() {
        assert!(matches_like("Hello", "hello"));
        assert!(matches_like("hello", "HELLO"));
        assert!(matches_like("ALICE@X.COM", "alice@%"));
        assert!(matches_like("Bob", "b_b"));
        // Non-letters and structure still matter.
        assert!(!matches_like("hellp", "hello"));
    }

    /// `%` matches across line breaks, as it does in SQL.
    #[test]
    fn matches_like_spans_newlines() {
        assert!(matches_like("line one\nline two", "line%two"));
    }

    /// A backslash escapes `%` and `_` in a `like` pattern, as `ESCAPE '\\'`
    /// does in SQL.
    #[test]
    fn matches_like_honors_backslash_escapes() {
        assert!(matches_like("100%", "100\\%"));
        assert!(!matches_like("1000", "100\\%"));
        assert!(matches_like("a_b", "a\\_b"));
        assert!(!matches_like("axb", "a\\_b"));
    }
}
