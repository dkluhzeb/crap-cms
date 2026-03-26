//! URL query parameter utilities — parsing, encoding, and validation for
//! `where[field][op]=value` filter parameters and sort/pagination URLs.

use crate::{
    core::{collection::CollectionDefinition, field::FieldType},
    db::query::{Filter, FilterClause, FilterOp},
};

/// Parse `where[field][op]=value` parameters from a raw query string.
/// Returns empty vec for malformed/invalid params. Best-effort parsing.
pub(crate) fn parse_where_params(raw_query: &str, def: &CollectionDefinition) -> Vec<FilterClause> {
    let mut filters = Vec::new();
    let system_cols = ["id", "created_at", "updated_at", "_status"];

    for part in raw_query.split('&') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };

        let value = url_decode(value);

        // Match where[field][op]
        let key = url_decode(key);

        let Some(rest) = key.strip_prefix("where[") else {
            continue;
        };

        let Some((field, rest)) = rest.split_once("][") else {
            continue;
        };

        let Some(op_str) = rest.strip_suffix(']') else {
            continue;
        };

        // Validate field exists
        let field_valid =
            system_cols.contains(&field) || def.fields.iter().any(|f| f.name == field);

        if !field_valid {
            continue;
        }

        let op = match op_str {
            "equals" => FilterOp::Equals(value),
            "not_equals" => FilterOp::NotEquals(value),
            "contains" => FilterOp::Contains(value),
            "like" => FilterOp::Like(value),
            "gt" => FilterOp::GreaterThan(value),
            "lt" => FilterOp::LessThan(value),
            "gte" => FilterOp::GreaterThanOrEqual(value),
            "lte" => FilterOp::LessThanOrEqual(value),
            "exists" => FilterOp::Exists,
            "not_exists" => FilterOp::NotExists,
            _ => continue,
        };

        filters.push(FilterClause::Single(Filter {
            field: field.to_string(),
            op,
        }));
    }

    filters
}

/// Simple percent-decoding for URL query values.
///
/// Collects decoded bytes into a `Vec<u8>` then converts via `String::from_utf8_lossy`
/// so multi-byte UTF-8 sequences (e.g. `%C3%A9` → `é`) decode correctly.
/// Malformed `%XX` sequences are preserved literally instead of being silently dropped.
pub(crate) fn url_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut iter = s.bytes();

    while let Some(b) = iter.next() {
        if b == b'+' {
            bytes.push(b' ');
        } else if b == b'%' {
            let hi = iter.next();
            let lo = hi.and_then(|_| iter.next());

            match (
                hi.and_then(|c| (c as char).to_digit(16)),
                lo.and_then(|c| (c as char).to_digit(16)),
            ) {
                (Some(h), Some(l)) => bytes.push((h * 16 + l) as u8),
                _ => {
                    // Malformed %XX — preserve the literal characters
                    bytes.push(b'%');
                    if let Some(h) = hi {
                        bytes.push(h);
                    }
                    if let Some(l) = lo {
                        bytes.push(l);
                    }
                }
            }
        } else {
            bytes.push(b);
        }
    }

    String::from_utf8_lossy(&bytes).into_owned()
}

/// Validate a sort field name against the collection definition.
/// Strips leading `-` (descending) before validation.
/// Returns the validated sort string (with `-` prefix if present), or None.
pub(crate) fn validate_sort(sort: &str, def: &CollectionDefinition) -> Option<String> {
    let field_name = sort.strip_prefix('-').unwrap_or(sort);
    let system_cols = ["id", "created_at", "updated_at", "_status"];
    let valid = system_cols.contains(&field_name)
        || def
            .fields
            .iter()
            .any(|f| f.name == field_name && is_column_eligible(&f.field_type));
    if valid { Some(sort.to_string()) } else { None }
}

/// Check if a field type is eligible for display as a list column.
pub(crate) fn is_column_eligible(field_type: &FieldType) -> bool {
    matches!(
        field_type,
        FieldType::Text
            | FieldType::Email
            | FieldType::Number
            | FieldType::Select
            | FieldType::Checkbox
            | FieldType::Date
            | FieldType::Relationship
            | FieldType::Textarea
            | FieldType::Radio
            | FieldType::Upload
    )
}

/// Build a list URL preserving all query params (pagination, search, sort, filters).
///
/// For page-based pagination, includes `page=N`. For cursor-based pagination,
/// pass `page=0` and provide a cursor string instead.
pub(crate) fn build_list_url(
    base: &str,
    page: i64,
    per_page: Option<i64>,
    search: Option<&str>,
    sort: Option<&str>,
    raw_where: &str,
) -> String {
    build_list_url_with_cursor(base, page, per_page, search, sort, raw_where, None)
}

/// Build a list URL with optional cursor parameter for cursor-based pagination.
pub(crate) fn build_list_url_with_cursor(
    base: &str,
    page: i64,
    per_page: Option<i64>,
    search: Option<&str>,
    sort: Option<&str>,
    raw_where: &str,
    cursor: Option<(&str, &str)>,
) -> String {
    let mut url = format!("{}?page={}", base, page);

    if let Some(pp) = per_page {
        url.push_str(&format!("&per_page={}", pp));
    }

    if let Some(s) = search {
        url.push_str(&format!("&search={}", url_encode(s)));
    }

    if let Some(s) = sort {
        url.push_str(&format!("&sort={}", url_encode(s)));
    }

    if let Some((param, value)) = cursor {
        url.push_str(&format!("&{}={}", param, url_encode(value)));
    }

    // Preserve where params from original query string
    for part in raw_where.split('&') {
        if part.starts_with("where%5B") || part.starts_with("where[") {
            url.push('&');
            url.push_str(part);
        }
    }
    url
}

/// Simple percent-encoding for URL query values.
fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                format!("{}", b as char)
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect()
}

/// Extract only `where[...]` params from a raw query string (for pagination link preservation).
pub(crate) fn extract_where_params(raw_query: &str) -> String {
    raw_query
        .split('&')
        .filter(|p| p.starts_with("where%5B") || p.starts_with("where["))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{collection::*, field::FieldDefinition};

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Select).build(),
            FieldDefinition::builder("body", FieldType::Richtext).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
        ];
        def
    }

    // --- parse_where_params tests ---

    #[test]
    fn parse_where_empty_query() {
        let def = test_def();
        let result = parse_where_params("", &def);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_equals_filter() {
        let def = test_def();
        let result = parse_where_params("where[title][equals]=hello", &def);
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "title");
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "hello"));
            }
            _ => panic!("Expected Single filter"),
        }
    }

    #[test]
    fn parse_where_multiple_filters() {
        let def = test_def();
        let result = parse_where_params("where[title][contains]=foo&where[count][gt]=5", &def);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_where_invalid_field_ignored() {
        let def = test_def();
        let result = parse_where_params("where[nonexistent][equals]=foo", &def);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_invalid_op_ignored() {
        let def = test_def();
        let result = parse_where_params("where[title][invalid]=foo", &def);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_system_column() {
        let def = test_def();
        let result = parse_where_params("where[created_at][gt]=2024-01-01", &def);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_where_exists_op() {
        let def = test_def();
        let result = parse_where_params("where[title][exists]=", &def);
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => assert!(matches!(f.op, FilterOp::Exists)),
            _ => panic!("Expected Single"),
        }
    }

    #[test]
    fn parse_where_encoded_value() {
        let def = test_def();
        let result = parse_where_params("where[title][equals]=hello%20world", &def);
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => {
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "hello world"));
            }
            _ => panic!("Expected Single"),
        }
    }

    // --- validate_sort tests ---

    #[test]
    fn validate_sort_valid_field() {
        let def = test_def();
        assert_eq!(validate_sort("title", &def), Some("title".to_string()));
    }

    #[test]
    fn validate_sort_descending() {
        let def = test_def();
        assert_eq!(validate_sort("-title", &def), Some("-title".to_string()));
    }

    #[test]
    fn validate_sort_system_col() {
        let def = test_def();
        assert_eq!(
            validate_sort("-created_at", &def),
            Some("-created_at".to_string())
        );
    }

    #[test]
    fn validate_sort_invalid() {
        let def = test_def();
        assert_eq!(validate_sort("nonexistent", &def), None);
    }

    #[test]
    fn validate_sort_ineligible_field() {
        let def = test_def();
        // body is Richtext — not column-eligible
        assert_eq!(validate_sort("body", &def), None);
    }

    // --- build_list_url tests ---

    #[test]
    fn build_list_url_basic() {
        let url = build_list_url("/admin/collections/posts", 2, None, None, None, "");
        assert_eq!(url, "/admin/collections/posts?page=2");
    }

    #[test]
    fn build_list_url_with_search_sort() {
        let url = build_list_url(
            "/admin/collections/posts",
            1,
            None,
            Some("hello"),
            Some("-title"),
            "",
        );
        assert!(url.contains("search=hello"));
        assert!(url.contains("sort=-title"));
    }

    #[test]
    fn build_list_url_preserves_where() {
        let url = build_list_url(
            "/admin/collections/posts",
            1,
            None,
            None,
            None,
            "where[title][equals]=foo&page=1",
        );
        assert!(url.contains("where[title][equals]=foo"));
        assert!(!url.contains("page=1&page=1")); // should not duplicate page
    }

    #[test]
    fn build_list_url_with_cursor_param() {
        let url = build_list_url_with_cursor(
            "/admin/collections/posts",
            1,
            None,
            None,
            Some("title"),
            "",
            Some(("after_cursor", "abc123")),
        );
        assert!(url.contains("after_cursor=abc123"));
        assert!(url.contains("sort=title"));
        assert!(url.contains("page=1"));
    }

    #[test]
    fn build_list_url_with_cursor_none() {
        let url =
            build_list_url_with_cursor("/admin/collections/posts", 2, None, None, None, "", None);
        assert_eq!(url, "/admin/collections/posts?page=2");
        assert!(!url.contains("cursor"));
    }

    // --- is_column_eligible tests ---

    #[test]
    fn column_eligible_text() {
        assert!(is_column_eligible(&FieldType::Text));
        assert!(is_column_eligible(&FieldType::Email));
        assert!(is_column_eligible(&FieldType::Number));
        assert!(is_column_eligible(&FieldType::Select));
        assert!(is_column_eligible(&FieldType::Checkbox));
        assert!(is_column_eligible(&FieldType::Date));
    }

    #[test]
    fn column_ineligible_richtext() {
        assert!(!is_column_eligible(&FieldType::Richtext));
        assert!(!is_column_eligible(&FieldType::Array));
        assert!(!is_column_eligible(&FieldType::Group));
        assert!(!is_column_eligible(&FieldType::Blocks));
        assert!(!is_column_eligible(&FieldType::Json));
        assert!(!is_column_eligible(&FieldType::Code));
        assert!(!is_column_eligible(&FieldType::Join));
    }

    // --- url_decode tests ---

    #[test]
    fn url_decode_basic() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("foo+bar"), "foo bar");
        assert_eq!(url_decode("plain"), "plain");
    }

    #[test]
    fn url_decode_multibyte_utf8() {
        // é = U+00E9 = 0xC3 0xA9
        assert_eq!(url_decode("%C3%A9"), "é");
    }

    #[test]
    fn url_decode_cjk() {
        // 中 = U+4E2D = 0xE4 0xB8 0xAD
        assert_eq!(url_decode("%E4%B8%AD"), "中");
    }

    #[test]
    fn url_decode_emoji() {
        // 😀 = U+1F600 = 0xF0 0x9F 0x98 0x80
        assert_eq!(url_decode("%F0%9F%98%80"), "😀");
    }

    #[test]
    fn url_decode_invalid_hex_preserves() {
        assert_eq!(url_decode("%ZZ"), "%ZZ");
    }

    #[test]
    fn url_decode_trailing_percent() {
        assert_eq!(url_decode("test%"), "test%");
    }

    #[test]
    fn url_decode_partial_hex() {
        assert_eq!(url_decode("test%2"), "test%2");
    }
}
