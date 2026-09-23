//! Filter/where parameter parsing from URL query strings.
//!
//! Supports two URL grammars that together cover the full `Vec<FilterClause>` shape
//! the read pipeline accepts:
//! - `where[field][op]=value` — top-level AND clause (a single filter contributing to
//!   the implicit-AND list of clauses).
//! - `where[or][G][N][field][op]=value` — contributes to AND-bucket `N` of OR-clause
//!   `G`. Multiple entries with the same `(G, N)` AND together inside the bucket;
//!   different `N` values inside the same `G` are OR'd; different `G` values produce
//!   independent OR-clauses that are AND'd at the top level. Mirrors
//!   `FilterClause::Or(Vec<Vec<Filter>>)` once per `G`.
//!
//! Rows are combined exactly as written: every row of an AND-context (the
//! top level, or one OR-bucket) must match — two `equals` rows on the same field
//! ask for a value (a has-many list: elements) matching both, never "either".
//! "Any of" is an OR group: `where[or][G][0][f][equals]=a&where[or][G][1][f][equals]=b`.

use std::{
    collections::{BTreeMap, HashSet},
    fmt::{self, Display, Formatter},
};

use crate::{
    admin::Translations,
    core::collection::CollectionDefinition,
    db::query::{
        FILTER_OP_SPECS, Filter, FilterClause, FilterOp, FilterOpValueKind,
        get_valid_filter_columns,
    },
};

use super::url::url_decode;

/// Valid operator names for the URL grammar, listed in error messages on an
/// unknown operator. Derived from the canonical operator table
/// ([`FILTER_OP_SPECS`]) so the message can never advertise an operator the
/// decoder rejects — minus the list operators (`in`/`not_in`), which the URL
/// form does not spell: "any of" is an OR group of `equals` rows, "none of"
/// an AND of `not_equals` rows.
fn valid_url_ops() -> String {
    FILTER_OP_SPECS
        .iter()
        .filter(|spec| spec.value != FilterOpValueKind::ScalarList)
        .map(|spec| spec.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Parse an operator string and value into a `FilterOp` — the canonical grammar
/// shared with the gRPC/MCP surfaces (`in`/`not_in` are not spelled in the URL
/// form, so they aren't parsed here).
///
/// `exists`/`not_exists` accept the admin UI's canonical valueless form
/// (`where[field][exists]=`) or the literal `true` — anything else is a 400,
/// matching the wire and Lua parsers: `where[field][exists]=false` used to
/// silently read as the inverted `IS NOT NULL`.
fn parse_filter_op(op_str: &str, value: String) -> Result<FilterOp, String> {
    match op_str {
        "exists" | "not_exists" => {
            if !value.is_empty() && value != "true" {
                return Err(format!(
                    "'{op_str}' accepts only the value 'true' (got '{value}'); use 'not_exists' for IS NULL and 'exists' for IS NOT NULL"
                ));
            }

            Ok(if op_str == "exists" {
                FilterOp::Exists
            } else {
                FilterOp::NotExists
            })
        }
        _ => FilterOp::scalar_from_name(op_str, value).ok_or_else(|| {
            format!(
                "Unknown filter operator '{op_str}' (valid: {})",
                valid_url_ops()
            )
        }),
    }
}

/// Outcome of decoding a single `where…` query-string entry. Top-level AND rows
/// have `or_position = None`; OR rows carry their `(group, bucket)` coordinates so
/// the post-process pass can reassemble `FilterClause::Or` correctly.
struct ParsedRow {
    or_position: Option<(usize, usize)>,
    filter: Filter,
}

/// Parse a `where[field][op]` key (the AND form). Returns `(field, op_str)`.
/// Rejects keys that begin with `where[or]…` so the OR form falls through.
pub(super) fn parse_top_key(key: &str) -> Option<(String, String)> {
    let rest = key.strip_prefix("where[")?;
    if rest.starts_with("or][") {
        return None;
    }
    let (field, rest) = rest.split_once("][")?;
    let op_str = rest.strip_suffix(']')?;
    Some((field.to_string(), op_str.to_string()))
}

/// Parse a `where[or][G][N][field][op]` key. Returns `(group_index, bucket_index, field, op_str)`.
pub(super) fn parse_or_key(key: &str) -> Option<(usize, usize, String, String)> {
    let rest = key.strip_prefix("where[or][")?;
    let (group_str, rest) = rest.split_once("][")?;
    let group: usize = group_str.parse().ok()?;
    let (bucket_str, rest) = rest.split_once("][")?;
    let bucket: usize = bucket_str.parse().ok()?;
    let (field, rest) = rest.split_once("][")?;
    let op_str = rest.strip_suffix(']')?;
    Some((group, bucket, field.to_string(), op_str.to_string()))
}

/// Decode a single `&`-separated query entry. `Ok(None)` for entries that are
/// not user filters at all (other query params, plus the typed `_status`
/// path); a present-but-invalid `where[...]` entry — malformed key, unknown
/// field, system column, unknown operator — is a hard error so the list page
/// can return 400 instead of silently rendering wrong/unfiltered results.
fn parse_one_entry(part: &str, valid_cols: &HashSet<String>) -> Result<Option<ParsedRow>, String> {
    let known_cols = ["id", "created_at", "updated_at"];

    let Some((key, value)) = part.split_once('=') else {
        let key = url_decode(part);
        if key.starts_with("where[") {
            return Err(format!("Malformed filter parameter '{key}' (missing '=')"));
        }
        return Ok(None);
    };

    let key = url_decode(key);
    if !key.starts_with("where[") {
        return Ok(None);
    }
    let value = url_decode(value);

    let (or_position, field, op_str) = if let Some((g, n, f, o)) = parse_or_key(&key) {
        (Some((g, n)), f, o)
    } else if let Some((f, o)) = parse_top_key(&key) {
        (None, f, o)
    } else {
        return Err(format!("Malformed filter parameter '{key}'"));
    };

    // `_status` rides the typed extractor (`extract_status_filter`), not the
    // generic path — see the doc comment there. Only `equals` is supported.
    if field == "_status" {
        if op_str == "equals" {
            return Ok(None);
        }
        return Err(format!(
            "Unsupported filter operator '{op_str}' on '_status' (only 'equals')"
        ));
    }

    if field.starts_with('_') {
        return Err(format!("Cannot filter on system column '{field}'"));
    }

    // Validate against the same filterable-column set the read pipeline uses
    // (`get_valid_filter_columns`), so the admin `where[]` grammar accepts a
    // group sub-field column (`seo__title`) exactly like the sort path and the
    // service layer do — instead of a flat top-level-only scan that rejected
    // nested fields (and accepted array/blocks columns the service then rejected).
    let field_valid = known_cols.contains(&field.as_str()) || valid_cols.contains(&field);
    if !field_valid {
        return Err(format!("Unknown filter field '{field}'"));
    }

    let op = parse_filter_op(&op_str, value)?;

    Ok(Some(ParsedRow {
        or_position,
        filter: Filter { field, op },
    }))
}

/// Translation key of the message for a `_status` row inside a mixed OR
/// group. The admin filter builder shows the same text as its hint for why
/// such a row can only join its neighbour with AND.
pub(crate) const STATUS_IN_MIXED_OR_KEY: &str = "filter_status_or_mixed";

/// Why a list URL's `where[…]` parameters were refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WhereParamsError {
    /// A `_status` row shares an OR group with another field — see
    /// `reject_status_in_mixed_or`. The filter builder cannot produce this
    /// shape, so it reaches the server only from a hand-edited URL; its
    /// message is translated into the viewer's UI language.
    StatusInMixedOr,
    /// Any other invalid entry (malformed key, unknown field or operator,
    /// system column), with its diagnostic message.
    Invalid(String),
}

impl WhereParamsError {
    /// The message for a viewer whose UI language is `locale`.
    pub(crate) fn message(&self, translations: &Translations, locale: &str) -> String {
        match self {
            Self::StatusInMixedOr => translations.get(locale, STATUS_IN_MIXED_OR_KEY).to_string(),
            Self::Invalid(message) => message.clone(),
        }
    }
}

impl From<String> for WhereParamsError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

impl Display for WhereParamsError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::StatusInMixedOr => f.write_str(
                "'_status' cannot be combined with other fields in an OR filter group; \
                 filter the status on its own row",
            ),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

/// Which rows one OR group (`where[or][G]…`) holds: its bucket indices and
/// whether any row filters `_status` / any other field.
#[derive(Default)]
struct OrGroupShape {
    buckets: HashSet<usize>,
    has_status: bool,
    has_other: bool,
}

/// Refuse `_status` inside an OR group that also filters another field.
///
/// `_status` does not ride the generic filter tree: it is lifted into the
/// typed status filter, which is combined with the whole query by AND. Inside a real OR
/// (two or more buckets) next to another field that lift changes the meaning
/// — `(_status = draft) OR (title = B)` would run as `draft AND title = B` —
/// so the combination is a 400. An OR group of only `_status` rows is a
/// status union, and a single-bucket group is an AND; both are exact.
fn reject_status_in_mixed_or(raw_query: &str) -> Result<(), WhereParamsError> {
    let mut groups: BTreeMap<usize, OrGroupShape> = BTreeMap::new();

    for part in raw_query.split('&') {
        let key = url_decode(part.split('=').next().unwrap_or(""));
        let Some((group, bucket, field, _)) = parse_or_key(&key) else {
            continue;
        };

        let shape = groups.entry(group).or_default();
        shape.buckets.insert(bucket);

        if field == "_status" {
            shape.has_status = true;
        } else {
            shape.has_other = true;
        }
    }

    let mixed = groups
        .values()
        .any(|g| g.buckets.len() > 1 && g.has_status && g.has_other);
    if mixed {
        return Err(WhereParamsError::StatusInMixedOr);
    }

    Ok(())
}

/// Parse `where[field][op]=value` and `where[or][N][field][op]=value` parameters from
/// a raw URL query string. Returns the resulting `Vec<FilterClause>` ready for the
/// service-layer read pipeline. Strict: a present-but-invalid `where[...]` entry
/// (malformed key, unknown field, system column, unknown operator, `_status` in a
/// mixed OR group) is an `Err` so the caller can return 400 instead of silently
/// rendering wrong/unfiltered results.
///
/// The per-entry decode rejects system columns (`_*`) so they cannot ride the
/// generic path; the service-layer read entrypoints
/// ([`find_documents`](crate::service::find_documents) /
/// [`count_documents`](crate::service::count_documents)) apply the same rejection
/// uniformly across every read surface.
pub(crate) fn parse_where_params(
    raw_query: &str,
    def: &CollectionDefinition,
) -> Result<Vec<FilterClause>, WhereParamsError> {
    reject_status_in_mixed_or(raw_query)?;

    // The filterable-column set (id + leaf columns incl. `group__sub`, minus
    // array/blocks), computed once and shared across every `where[]` entry.
    let valid_cols = get_valid_filter_columns(def, None);

    let parsed: Vec<ParsedRow> = raw_query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|part| parse_one_entry(part, &valid_cols))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();

    // Partition into top-level AND rows and OR rows grouped by `(group, bucket)`.
    // BTreeMap keeps groups + buckets in numeric URL order so output is stable.
    let mut top_level: Vec<Filter> = Vec::new();
    let mut or_groups: BTreeMap<usize, BTreeMap<usize, Vec<Filter>>> = BTreeMap::new();
    for r in parsed {
        match r.or_position {
            None => top_level.push(r.filter),
            Some((group, bucket)) => or_groups
                .entry(group)
                .or_default()
                .entry(bucket)
                .or_default()
                .push(r.filter),
        }
    }

    let mut clauses: Vec<FilterClause> = top_level.into_iter().map(FilterClause::Single).collect();

    for (_group_idx, buckets) in or_groups {
        let groups: Vec<Vec<Filter>> = buckets.into_values().filter(|g| !g.is_empty()).collect();
        match groups.len() {
            0 => {}
            // One bucket is degenerate (no real OR) — flatten its filters back into
            // top-level AND clauses so SQL doesn't carry a `(x)` wrapper for nothing.
            1 => {
                for f in groups.into_iter().next().unwrap() {
                    clauses.push(FilterClause::Single(f));
                }
            }
            _ => clauses.push(FilterClause::or_groups(groups)),
        }
    }

    Ok(clauses)
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
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::core::{FieldType, collection::CollectionDefinition, field::FieldDefinition};

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Select).build(),
            FieldDefinition::builder("body", FieldType::Richtext).build(),
            FieldDefinition::builder("count", FieldType::Number).build(),
            FieldDefinition::builder("slug", FieldType::Text).build(),
        ];
        def
    }

    #[test]
    fn parse_where_empty_query() {
        let def = test_def();
        let result = parse_where_params("", &def).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_equals_filter() {
        let def = test_def();
        let result = parse_where_params("where[title][equals]=hello", &def).unwrap();
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
        let result = parse_where_params(
            "where[title][contains]=foo&where[count][greater_than]=5",
            &def,
        )
        .unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_where_invalid_field_rejected() {
        let def = test_def();
        let err = parse_where_params("where[nonexistent][equals]=foo", &def)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Unknown filter field 'nonexistent'"),
            "unexpected: {err}"
        );
    }

    /// Regression: a group sub-field column (`seo__title`) is a valid filter
    /// field — the admin `where[]` grammar must accept it like the sort path and
    /// the service layer do (it used to reject nested fields via a flat scan).
    #[test]
    fn parse_where_accepts_group_subfield() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        let result = parse_where_params("where[seo__title][equals]=hi", &def).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => {
                assert_eq!(f.field, "seo__title");
                assert!(matches!(f.op, FilterOp::Equals(_)));
            }
            other => panic!("expected single clause, got {other:?}"),
        }

        // A genuinely unknown nested column is still rejected.
        let err = parse_where_params("where[seo__missing][equals]=x", &def)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Unknown filter field"), "unexpected: {err}");
    }

    #[test]
    fn parse_where_invalid_op_rejected() {
        let def = test_def();
        let err = parse_where_params("where[title][invalid]=foo", &def)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Unknown filter operator 'invalid'") && err.contains("equals"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn parse_where_status_equals_skips_to_typed_extractor() {
        let def = test_def();
        // `_status` + equals is the typed-extractor path — NOT an error and
        // NOT a generic filter.
        let result = parse_where_params("where[_status][equals]=draft", &def).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_status_non_equals_rejected() {
        let def = test_def();
        let err = parse_where_params("where[_status][not_equals]=draft", &def)
            .unwrap_err()
            .to_string();
        assert!(err.contains("only 'equals'"), "unexpected: {err}");
    }

    #[test]
    fn parse_where_system_column_rejected() {
        let def = test_def();
        let err = parse_where_params("where[_deleted_at][exists]=", &def)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("system column '_deleted_at'"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn parse_where_malformed_or_key_rejected() {
        let def = test_def();
        let err = parse_where_params("where[or][abc][0][title][equals]=x", &def)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Malformed filter parameter"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn parse_where_missing_value_rejected() {
        let def = test_def();
        let err = parse_where_params("where[title][equals]", &def)
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing '='"), "unexpected: {err}");
    }

    #[test]
    fn parse_where_other_params_skipped() {
        let def = test_def();
        let result = parse_where_params("page=2&sort=-title&search=foo&trash=1", &def).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_where_system_column() {
        let def = test_def();
        // `created_at` is a known timestamp column (not underscore-prefixed) and is still filterable
        let result =
            parse_where_params("where[created_at][greater_than]=2024-01-01", &def).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_where_exists_op() {
        let def = test_def();
        let result = parse_where_params("where[title][exists]=", &def).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => assert!(matches!(f.op, FilterOp::Exists)),
            _ => panic!("Expected Single"),
        }
    }

    #[test]
    fn parse_where_encoded_value() {
        let def = test_def();
        let result = parse_where_params("where[title][equals]=hello%20world", &def).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Single(f) => {
                assert!(matches!(&f.op, FilterOp::Equals(v) if v == "hello world"));
            }
            _ => panic!("Expected Single"),
        }
    }

    /// The operator of every top-level row, in URL order.
    fn top_level_ops(result: &[FilterClause]) -> Vec<&FilterOp> {
        result
            .iter()
            .map(|clause| match clause {
                FilterClause::Single(f) => &f.op,
                other => panic!("expected Single, got {other:?}"),
            })
            .collect()
    }

    /// Regression: two `equals` rows on one field were merged into `in`, so
    /// the filter builder's "title is foo AND title is bar" listed documents
    /// matching either value — and on a has-many list, documents holding
    /// either element instead of both. AND rows now stay AND.
    #[test]
    fn parse_where_two_equals_same_field_stay_two_and_rows() {
        let def = test_def();
        let result =
            parse_where_params("where[title][equals]=foo&where[title][equals]=bar", &def).unwrap();

        let ops = top_level_ops(&result);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], FilterOp::Equals(v) if v == "foo"));
        assert!(matches!(ops[1], FilterOp::Equals(v) if v == "bar"));
    }

    #[test]
    fn parse_where_two_not_equals_same_field_stay_two_and_rows() {
        let def = test_def();
        let result = parse_where_params(
            "where[title][not_equals]=a&where[title][not_equals]=b",
            &def,
        )
        .unwrap();

        let ops = top_level_ops(&result);
        assert_eq!(ops.len(), 2);
        assert!(ops.iter().all(|op| matches!(op, FilterOp::NotEquals(_))));
    }

    /// "Any of" is an OR group of `equals` rows.
    #[test]
    fn parse_where_any_of_is_an_or_group() {
        let def = test_def();
        let result = parse_where_params(
            "where[or][0][0][status][equals]=a&where[or][0][1][status][equals]=b",
            &def,
        )
        .unwrap();

        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], FilterClause::Or(alts) if alts.len() == 2));
    }

    #[test]
    fn parse_where_same_field_different_ops_keeps_separate() {
        let def = test_def();
        // `equals=a` is mergeable, `contains=b` is not; result has both as separate
        // ANDed Singles.
        let result =
            parse_where_params("where[title][equals]=a&where[title][contains]=b", &def).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_where_or_two_buckets() {
        let def = test_def();
        // Single Or-clause (group 0), two buckets — `(title=A) OR (slug=B)`.
        let result = parse_where_params(
            "where[or][0][0][title][equals]=A&where[or][0][1][slug][equals]=B",
            &def,
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Or(alts) => {
                assert_eq!(alts.len(), 2);
                let (FilterClause::Single(f0), FilterClause::Single(f1)) = (&alts[0], &alts[1])
                else {
                    panic!("expected single-filter buckets, got {result:?}");
                };
                assert_eq!(f0.field, "title");
                assert_eq!(f1.field, "slug");
            }
            _ => panic!("expected Or, got {result:?}"),
        }
    }

    #[test]
    fn parse_where_or_with_inner_and() {
        let def = test_def();
        // Two filters in the same OR-bucket (group 0, bucket 0) AND together.
        let result = parse_where_params(
            "where[or][0][0][title][equals]=A&where[or][0][0][slug][equals]=p&where[or][0][1][title][equals]=B",
            &def,
        ).unwrap();
        assert_eq!(result.len(), 1);
        match &result[0] {
            FilterClause::Or(alts) => {
                assert_eq!(alts.len(), 2);
                let FilterClause::And(b0) = &alts[0] else {
                    panic!("bucket 0 should AND two filters");
                };
                assert_eq!(b0.len(), 2, "bucket 0 has two AND'd filters");
                assert!(
                    matches!(&alts[1], FilterClause::Single(_)),
                    "bucket 1 is a single filter"
                );
            }
            _ => panic!("expected Or"),
        }
    }

    #[test]
    fn parse_where_or_plus_top_level_and() {
        let def = test_def();
        let result = parse_where_params(
            "where[title][equals]=hello&where[or][0][0][slug][equals]=a&where[or][0][1][slug][equals]=b",
            &def,
        ).unwrap();
        assert_eq!(result.len(), 2, "top-level Single + one Or-clause");
        assert!(matches!(&result[0], FilterClause::Single(_)));
        assert!(matches!(&result[1], FilterClause::Or(g) if g.len() == 2));
    }

    /// Two `equals` rows on one field inside an OR bucket AND together, as
    /// they do at the top level.
    #[test]
    fn parse_where_same_field_rows_within_or_bucket_stay_and() {
        let def = test_def();
        let result = parse_where_params(
            "where[or][0][0][title][equals]=A&where[or][0][0][title][equals]=B&where[or][0][1][slug][equals]=c",
            &def,
        ).unwrap();
        match &result[0] {
            FilterClause::Or(alts) => {
                assert_eq!(alts.len(), 2);
                let FilterClause::And(bucket) = &alts[0] else {
                    panic!("bucket 0 ANDs its two equals rows, got {:?}", alts[0]);
                };
                assert_eq!(bucket.len(), 2);
            }
            _ => panic!("expected Or"),
        }
    }

    #[test]
    fn parse_where_or_single_bucket_flattens_to_and() {
        let def = test_def();
        // Single bucket inside a single group → no real OR. Flatten so SQL
        // doesn't wrap with a no-op `(x)`.
        let result = parse_where_params("where[or][0][0][title][equals]=A", &def).unwrap();
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], FilterClause::Single(_)));
    }

    #[test]
    fn parse_where_two_or_clauses() {
        let def = test_def();
        // `(title=A OR title=B) AND (slug=C OR slug=D)` — two independent Or
        // groups (G=0 and G=1).
        let result = parse_where_params(
            "where[or][0][0][title][equals]=A&where[or][0][1][title][equals]=B\
             &where[or][1][0][slug][equals]=C&where[or][1][1][slug][equals]=D",
            &def,
        )
        .unwrap();
        assert_eq!(result.len(), 2, "two independent Or-clauses");
        for clause in &result {
            assert!(matches!(clause, FilterClause::Or(g) if g.len() == 2));
        }
    }

    #[test]
    fn parse_where_or_bucket_url_encoded() {
        let def = test_def();
        let result = parse_where_params(
            "where%5Bor%5D%5B0%5D%5B0%5D%5Btitle%5D%5Bequals%5D=A&where%5Bor%5D%5B0%5D%5B1%5D%5Btitle%5D%5Bequals%5D=B",
            &def,
        ).unwrap();
        match &result[0] {
            FilterClause::Or(groups) => assert_eq!(groups.len(), 2),
            _ => panic!("expected Or, got {result:?}"),
        }
    }

    /// Regression: `(_status = draft) OR (title = B)` silently became
    /// `_status = draft AND title = B` — the status row was lifted out of its
    /// OR bucket into a global filter. Mixing `_status` with other fields in
    /// one OR group is refused instead.
    #[test]
    fn parse_where_rejects_status_mixed_into_an_or_group() {
        let def = test_def();
        let err = parse_where_params(
            "where[or][0][0][_status][equals]=draft&where[or][0][1][title][equals]=B",
            &def,
        )
        .unwrap_err();

        assert_eq!(err, WhereParamsError::StatusInMixedOr);
        assert!(err.to_string().contains("_status"), "{err}");
    }

    /// The mixed-OR refusal renders in the viewer's UI language, from the
    /// same translation key the filter builder's hint uses.
    #[test]
    fn status_in_mixed_or_message_is_translated() {
        let translations = Translations::load(Path::new("/nonexistent"));
        let err = WhereParamsError::StatusInMixedOr;

        let english = err.message(&translations, "en");
        let german = err.message(&translations, "de");

        assert_eq!(english, translations.get("en", STATUS_IN_MIXED_OR_KEY));
        assert_eq!(german, translations.get("de", STATUS_IN_MIXED_OR_KEY));
        assert_ne!(english, STATUS_IN_MIXED_OR_KEY, "key missing from en.json");
        assert_ne!(german, english, "key not translated in de.json");
    }

    /// Every other refusal keeps its diagnostic message as is.
    #[test]
    fn invalid_where_message_is_passed_through() {
        let translations = Translations::load(Path::new("/nonexistent"));
        let err = WhereParamsError::from("Unknown filter field 'x'".to_string());

        assert_eq!(err.message(&translations, "de"), "Unknown filter field 'x'");
    }

    /// The URL-encoded form is refused the same way.
    #[test]
    fn parse_where_rejects_encoded_status_mixed_into_an_or_group() {
        let def = test_def();
        let query = "where%5Bor%5D%5B0%5D%5B0%5D%5B_status%5D%5Bequals%5D=draft\
                     &where%5Bor%5D%5B0%5D%5B1%5D%5Btitle%5D%5Bequals%5D=B";

        assert!(parse_where_params(query, &def).is_err());
    }

    /// An OR group of only `_status` rows is a plain status union, and a
    /// single-bucket group is an AND — both stay accepted.
    #[test]
    fn parse_where_accepts_status_only_or_groups_and_single_buckets() {
        let def = test_def();

        let status_only = parse_where_params(
            "where[or][0][0][_status][equals]=draft&where[or][0][1][_status][equals]=published",
            &def,
        )
        .unwrap();
        assert!(status_only.is_empty());

        let single_bucket = parse_where_params(
            "where[or][0][0][_status][equals]=draft&where[or][0][0][title][equals]=B",
            &def,
        )
        .unwrap();
        assert_eq!(single_bucket.len(), 1);

        let separate_groups = parse_where_params(
            "where[or][0][0][_status][equals]=draft&where[or][0][1][_status][equals]=published\
             &where[or][1][0][title][equals]=A&where[or][1][1][title][equals]=B",
            &def,
        )
        .unwrap();
        assert_eq!(separate_groups.len(), 1);
    }
}

#[cfg(test)]
mod exists_value_tests {
    use super::*;

    /// Regression: `exists=false` in a list URL used to decode as
    /// `FilterOp::Exists` (IS NOT NULL) — the inversion the wire/Lua
    /// parsers reject as a hard error.
    #[test]
    fn exists_accepts_only_the_literal_true() {
        assert!(matches!(
            parse_filter_op("exists", "true".to_string()),
            Ok(FilterOp::Exists)
        ));
        assert!(matches!(
            parse_filter_op("not_exists", "true".to_string()),
            Ok(FilterOp::NotExists)
        ));

        // The admin UI serializes these ops valueless (`where[x][exists]=`).
        assert!(matches!(
            parse_filter_op("not_exists", String::new()),
            Ok(FilterOp::NotExists)
        ));

        for (op, val) in [("exists", "false"), ("not_exists", "0"), ("exists", "1")] {
            let err = parse_filter_op(op, val.to_string()).unwrap_err();
            assert!(
                err.contains("accepts only the value 'true'"),
                "{op}={val}: {err}"
            );
        }

        let err = parse_filter_op("banana", "x".to_string()).unwrap_err();
        assert!(err.contains("Unknown filter operator"), "{err}");
    }
}
