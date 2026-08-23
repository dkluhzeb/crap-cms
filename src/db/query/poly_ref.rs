//! The polymorphic-reference wire grammar `"collection/id"`.
//!
//! One parser and one formatter so every poly-ref reader/writer agrees on the
//! grammar. Three hand-rolled parsers had already diverged — two rejected an
//! empty half (`"col/"` / `"/id"`), the back-reference reader silently accepted
//! them — so a malformed ref could be dropped on one path and kept on another.

/// Parse `"collection/id"` into `(collection, id)`. Requires a `/` separator and
/// a non-empty collection **and** id; returns `None` otherwise. Slicing is on a
/// `char`-boundary split so multi-byte UTF-8 never panics.
pub(crate) fn parse(s: &str) -> Option<(String, String)> {
    let (collection, id) = s.split_once('/')?;

    if collection.is_empty() || id.is_empty() {
        return None;
    }

    Some((collection.to_string(), id.to_string()))
}

/// Format a `(collection, id)` pair as the `"collection/id"` wire string — the
/// inverse of [`parse`], so the read and write grammar can't drift.
#[must_use]
pub(crate) fn format(collection: &str, id: &str) -> String {
    format!("{collection}/{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        assert_eq!(
            parse("articles/a1"),
            Some(("articles".to_string(), "a1".to_string()))
        );
        assert_eq!(parse("a/b"), Some(("a".to_string(), "b".to_string())));
    }

    #[test]
    fn parse_no_slash_returns_none() {
        assert_eq!(parse("noslash"), None);
        assert_eq!(parse(""), None);
    }

    /// The grammar that had DIVERGED: every reader must reject an empty half.
    #[test]
    fn parse_empty_half_returns_none() {
        assert_eq!(parse("/someid"), None, "empty collection");
        assert_eq!(parse("col/"), None, "empty id");
    }

    /// Regression: multi-byte UTF-8 in collection or id must not panic.
    #[test]
    fn parse_multibyte_utf8() {
        assert_eq!(
            parse("記事/id1"),
            Some(("記事".to_string(), "id1".to_string()))
        );
        assert_eq!(
            parse("posts/日本語id"),
            Some(("posts".to_string(), "日本語id".to_string()))
        );
    }

    #[test]
    fn format_is_parse_inverse() {
        assert_eq!(format("articles", "a1"), "articles/a1");
        assert_eq!(
            parse(&format("記事", "id1")),
            Some(("記事".into(), "id1".into()))
        );
    }
}
