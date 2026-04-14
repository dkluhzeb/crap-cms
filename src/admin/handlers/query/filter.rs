//! Filter/where parameter parsing from URL query strings.

use crate::{
    core::collection::CollectionDefinition,
    db::query::{Filter, FilterClause, FilterOp},
};

use super::url::url_decode;

/// Parse an operator string and value into a `FilterOp`.
fn parse_filter_op(op_str: &str, value: String) -> Option<FilterOp> {
    match op_str {
        "equals" => Some(FilterOp::Equals(value)),
        "not_equals" => Some(FilterOp::NotEquals(value)),
        "contains" => Some(FilterOp::Contains(value)),
        "like" => Some(FilterOp::Like(value)),
        "gt" => Some(FilterOp::GreaterThan(value)),
        "lt" => Some(FilterOp::LessThan(value)),
        "gte" => Some(FilterOp::GreaterThanOrEqual(value)),
        "lte" => Some(FilterOp::LessThanOrEqual(value)),
        "exists" => Some(FilterOp::Exists),
        "not_exists" => Some(FilterOp::NotExists),
        _ => None,
    }
}

/// Parse a single `where[field][op]=value` key-value pair into field name, op string, and value.
fn parse_where_key(key: &str, value: &str) -> Option<(String, String, String)> {
    let key = url_decode(key);
    let rest = key.strip_prefix("where[")?;
    let (field, rest) = rest.split_once("][")?;
    let op_str = rest.strip_suffix(']')?;

    Some((field.to_string(), op_str.to_string(), url_decode(value)))
}

/// Parse `where[field][op]=value` parameters from a raw query string.
/// Returns empty vec for malformed/invalid params. Best-effort parsing.
///
/// This function does NOT filter out system columns. The service-layer read
/// entrypoints ([`find_documents`](crate::service::find_documents) /
/// [`count_documents`](crate::service::count_documents)) apply the
/// `_*`-column rejection uniformly across every read surface.
pub(crate) fn parse_where_params(raw_query: &str, def: &CollectionDefinition) -> Vec<FilterClause> {
    let known_cols = ["id", "created_at", "updated_at"];

    raw_query
        .split('&')
        .filter_map(|part| {
            let (key, value) = part.split_once('=')?;
            let (field, op_str, value) = parse_where_key(key, value)?;

            let field_valid =
                known_cols.contains(&field.as_str()) || def.fields.iter().any(|f| f.name == field);

            if !field_valid {
                return None;
            }

            let op = parse_filter_op(&op_str, value)?;

            Some(FilterClause::Single(Filter { field, op }))
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
    use crate::core::field::FieldType;
    use crate::core::{collection::CollectionDefinition, field::FieldDefinition};

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
        // `created_at` is a known timestamp column (not underscore-prefixed) and is still filterable
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
}
