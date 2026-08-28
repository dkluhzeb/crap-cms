//! Shared helpers for collection CRUD tool implementations.

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};

use crate::{
    core::{
        CollectionDefinition, Document, DocumentFields, FieldDefinition, flatten_array_sub_fields,
    },
    db::query,
};

/// Read the optional `select` field projection from tool args: an array of
/// field-name strings. Non-string entries are ignored; absent or empty means
/// no projection. Parity with the gRPC/Lua `select` option.
pub(in crate::mcp::tools) fn parse_select(args: &Value) -> Option<Vec<String>> {
    let arr = args.get("select")?.as_array()?;

    let fields: Vec<String> = arr
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect();

    (!fields.is_empty()).then_some(fields)
}

/// Read the `events` write-tool flag from tool args, defaulting to `true`
/// (events are emitted unless the caller opts out). One source so every write
/// tool — collection and global — shares the same default and can't drift.
pub(in crate::mcp::tools) fn events_flag(args: &Value) -> bool {
    args.get("events").and_then(Value::as_bool).unwrap_or(true)
}

/// Pull the reserved top-level `password` from a write-tool object on an auth
/// collection (`None` for a non-auth collection, where `password` is ordinary
/// field data). One source so `create` / `update` / `create_many` extract it
/// identically instead of each re-deriving the `is_auth_collection()` guard.
///
/// `empty_as_none` selects the per-op treatment of an empty string: `update`
/// passes `true` (empty means "leave the password unchanged"); `create` and
/// `create_many` pass `false` (there is nothing to preserve, so an empty
/// password flows through to the policy validator and is rejected).
pub(in crate::mcp::tools) fn extract_auth_password(
    def: &CollectionDefinition,
    obj: &Value,
    empty_as_none: bool,
) -> Option<String> {
    if !def.is_auth_collection() {
        return None;
    }

    obj.get("password")
        .and_then(Value::as_str)
        .filter(|s| !(empty_as_none && s.is_empty()))
        .map(ToString::to_string)
}

/// Reserved top-level meta-keys for a single-document write tool — the keys
/// [`extract_data_from_args`] must skip so they are not treated as unknown field
/// data. One source so `create` / `update` / `validate` can't drift (validate
/// previously omitted `events`, rejecting a valid dry-run that passed it).
/// `include_id` is set on ops that accept a target `id` (update / validate);
/// `password` is reserved only on auth collections (a non-auth collection may
/// carry a legitimate `password` field, matching the Lua surface).
pub(in crate::mcp::tools) fn reserved_data_keys(
    def: &CollectionDefinition,
    include_id: bool,
) -> Vec<&'static str> {
    let mut keys = vec!["locale", "draft", "events"];
    if include_id {
        keys.push("id");
    }
    if def.is_auth_collection() {
        keys.push("password");
    }
    keys
}

/// Parse JSON `where` object into filter clauses.
/// Supports `{ field: "value" }` (equals) and `{ field: { op: value } }` (operator-based).
///
/// # Errors
///
/// Returns an error for an unknown filter operator (or a malformed value for
/// a scalar operator) rather than silently dropping the condition — a
/// dropped filter on an AI-driven surface would return more rows than the
/// caller asked for.
pub(in crate::mcp::tools) fn parse_where_filters(args: &Value) -> Result<Vec<query::FilterClause>> {
    let Some(where_obj) = args.get("where").and_then(|v| v.as_object()) else {
        return Ok(Vec::new());
    };

    let mut clauses = Vec::new();

    for (field, value) in where_obj {
        match value {
            Value::String(s) => {
                clauses.push(make_equals_clause(field, s.clone()));
            }
            Value::Number(n) => {
                clauses.push(make_equals_clause(field, n.to_string()));
            }
            Value::Bool(b) => {
                clauses.push(make_equals_clause(field, bool_to_string(*b)));
            }
            Value::Object(ops) => {
                parse_operator_filters(field, ops, &mut clauses)?;
            }
            // Fail loudly rather than silently drop the clause (a dropped filter
            // widens the match set — dangerous on delete_many/update_many). A
            // bare array like `{ status: ["a","b"] }` must use `{ status: { in:
            // [...] } }`.
            Value::Array(_) => bail!(
                "MCP where: field '{field}' has an array value; use an operator, \
                 e.g. {{ \"{field}\": {{ \"in\": [...] }} }}"
            ),
            Value::Null => bail!(
                "MCP where: field '{field}' has a null value; use the `exists` / \
                 `not_exists` operator instead"
            ),
        }
    }

    Ok(clauses)
}

/// Create an Equals filter clause for a field.
fn make_equals_clause(field: &str, value: String) -> query::FilterClause {
    query::FilterClause::Single(query::Filter {
        field: field.to_string(),
        op: query::FilterOp::Equals(value),
    })
}

/// Parse operator-based filters: `{ "greater_than": "50", "less_than": "100" }`.
fn parse_operator_filters(
    field: &str,
    ops: &Map<String, Value>,
    clauses: &mut Vec<query::FilterClause>,
) -> Result<()> {
    for (op_name, op_value) in ops {
        match op_name.as_str() {
            "in" | "not_in" => {
                let Some(arr) = op_value.as_array() else {
                    bail!(
                        "MCP where: operator '{op_name}' on field '{field}' needs an \
                         array value"
                    );
                };
                let vals: Vec<String> = arr
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect();
                let op = if op_name == "in" {
                    query::FilterOp::In(vals)
                } else {
                    query::FilterOp::NotIn(vals)
                };
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op,
                }));
            }
            "exists" => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op: query::FilterOp::Exists,
                }));
            }
            "not_exists" => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op: query::FilterOp::NotExists,
                }));
            }
            _ => {
                let val_str = match op_value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => bool_to_string(*b),
                    _ => bail!(
                        "MCP where: filter operator '{op_name}' on field '{field}' needs a \
                         string, number, or boolean value"
                    ),
                };
                let op = parse_scalar_op(op_name, val_str)?;
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op,
                }));
            }
        }
    }

    Ok(())
}

/// Parse a scalar filter operator name into a `FilterOp`.
///
/// # Errors
///
/// Returns an error for an unrecognized operator name, so a typo'd or
/// hallucinated operator fails loudly instead of silently dropping the
/// filter condition.
fn parse_scalar_op(op_name: &str, val: String) -> Result<query::FilterOp> {
    query::FilterOp::scalar_from_name(op_name, val).ok_or_else(|| {
        anyhow!(
            "MCP where: unknown filter operator '{op_name}'. Valid operators: equals, \
             not_equals, greater_than, greater_than_or_equal, less_than, less_than_or_equal, \
             like, contains, in, not_in, exists, not_exists"
        )
    })
}

/// Convert a bool to a SQLite-compatible `"1"` or `"0"` string.
fn bool_to_string(b: bool) -> String {
    if b { "1" } else { "0" }.to_string()
}

/// Convert a Document to a JSON Value — the top-level (untagged) envelope. Shares
/// the one `document_to_json` converter with the populate path (which passes the
/// `collection` tag for embedded refs), so the envelope key set can't drift.
pub(in crate::mcp::tools) fn doc_to_json(doc: &Document) -> Value {
    query::populate::document_to_json(doc, None)
}

/// Extract typed field data from JSON args, dropping `skip_keys` and `null`
/// values. Scalars and structured values both flow through as `Value` — the
/// typed write pipeline routes them to columns or join tables based on each
/// field's type.
///
/// A key that is neither a `skip_key` nor a declared top-level field of the
/// collection is **rejected** (rather than silently dropped by the field-driven
/// write pipeline) so a hallucinated/misspelled field name on this AI-driven
/// surface fails loudly. Layout wrappers (Row/Collapsible/Tabs) are transparent,
/// so their sub-fields are the valid top-level keys.
///
/// # Errors
///
/// Returns an error naming any key that is not a `skip_key` and not a field of
/// the collection.
pub(in crate::mcp::tools) fn extract_data_from_args(
    args: &Value,
    skip_keys: &[&str],
    fields: &[FieldDefinition],
) -> Result<DocumentFields> {
    let Some(obj) = args.as_object() else {
        return Ok(DocumentFields::new());
    };

    let known: HashSet<&str> = flatten_array_sub_fields(fields)
        .iter()
        .map(|f| f.name.as_str())
        .collect();

    let mut data = DocumentFields::new();

    for (k, v) in obj {
        if skip_keys.contains(&k.as_str()) || v.is_null() {
            continue;
        }

        if !known.contains(k.as_str()) {
            bail!("unknown field '{k}' for this collection");
        }

        data.insert(k.clone(), v.clone());
    }

    Ok(data)
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
    use std::collections::HashMap;

    use serde_json::{Map, Value, json};

    use super::*;
    use crate::{
        core::{DocumentFields, DocumentId, collection::Auth, document::Document},
        db::query,
    };

    /// Regression (C5): the shared password extractor preserves the intended
    /// create-vs-update asymmetry. `create`/`create_many` (`empty_as_none=false`)
    /// pass an empty string through so the policy validator rejects it; `update`
    /// (`empty_as_none=true`) treats empty as "no change"; a non-auth collection
    /// never extracts `password` (it is ordinary field data).
    #[test]
    fn extract_auth_password_asymmetry() {
        let mut auth_def = CollectionDefinition::new("users");
        auth_def.auth = Some(Auth::new(true));

        let empty = json!({ "password": "" });
        assert_eq!(
            extract_auth_password(&auth_def, &empty, false),
            Some(String::new()),
            "create: empty flows through to the policy validator"
        );
        assert_eq!(
            extract_auth_password(&auth_def, &empty, true),
            None,
            "update: empty means no change"
        );

        let real = json!({ "password": "secret" });
        assert_eq!(
            extract_auth_password(&auth_def, &real, true),
            Some("secret".to_string())
        );

        let plain_def = CollectionDefinition::new("posts");
        assert_eq!(
            extract_auth_password(&plain_def, &real, false),
            None,
            "non-auth collection: password is ordinary field data"
        );
    }

    // ── parse_where_filters: array operators ──────────────────────────────

    #[test]
    fn parse_where_in_operator() {
        let args = json!({
            "where": {
                "status": { "in": ["draft", "review"] }
            }
        });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "status");
                match &f.op {
                    query::FilterOp::In(vals) => assert_eq!(vals, &["draft", "review"]),
                    other => panic!("Expected In, got {other:?}"),
                }
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_not_in_operator() {
        let args = json!({
            "where": {
                "role": { "not_in": ["banned", "suspended"] }
            }
        });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "role");
                assert!(matches!(&f.op, query::FilterOp::NotIn(_)));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_exists_operator() {
        let args = json!({
            "where": {
                "avatar": { "exists": true }
            }
        });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "avatar");
                assert!(matches!(&f.op, query::FilterOp::Exists));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_not_exists_operator() {
        let args = json!({
            "where": {
                "deleted_at": { "not_exists": true }
            }
        });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::NotExists));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    // ── parse_where_filters: scalar field values ───────────────────────────

    #[test]
    fn parse_where_string_shorthand() {
        // { "field": "value" } → Equals
        let args = json!({ "where": { "title": "hello" } });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "title");
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "hello"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_number_shorthand() {
        let args = json!({ "where": { "count": 5 } });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "count");
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "5"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_bool_shorthand_true() {
        let args = json!({ "where": { "active": true } });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "1"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_bool_shorthand_false() {
        let args = json!({ "where": { "active": false } });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "0"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_scalar_operators() {
        for (op_name, expected_variant) in &[
            ("not_equals", "not_equals"),
            ("contains", "contains"),
            ("greater_than", "greater_than"),
            ("greater_than_or_equal", "greater_than_or_equal"),
            ("less_than", "less_than"),
            ("less_than_or_equal", "less_than_or_equal"),
            ("like", "like"),
        ] {
            let args = {
                let mut where_field = Map::new();
                where_field.insert(op_name.to_string(), json!("val"));
                let mut where_obj = Map::new();
                where_obj.insert("field".to_string(), Value::Object(where_field));
                let mut root = Map::new();
                root.insert("where".to_string(), Value::Object(where_obj));
                Value::Object(root)
            };
            let clauses = parse_where_filters(&args).unwrap();
            assert_eq!(
                clauses.len(),
                1,
                "operator {op_name} produced wrong clause count"
            );
            match &clauses[0] {
                query::FilterClause::Single(f) => {
                    let matched = matches!(
                        (&f.op, *expected_variant),
                        (query::FilterOp::NotEquals(_), "not_equals")
                            | (query::FilterOp::Contains(_), "contains")
                            | (query::FilterOp::GreaterThan(_), "greater_than")
                            | (
                                query::FilterOp::GreaterThanOrEqual(_),
                                "greater_than_or_equal"
                            )
                            | (query::FilterOp::LessThan(_), "less_than")
                            | (query::FilterOp::LessThanOrEqual(_), "less_than_or_equal")
                            | (query::FilterOp::Like(_), "like")
                    );
                    assert!(
                        matched,
                        "Wrong op variant for operator {}: got {:?}",
                        op_name, f.op
                    );
                }
                other => panic!("Expected Single for {op_name}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_where_scalar_op_with_number() {
        let args = json!({ "where": { "age": { "greater_than": 18 } } });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::GreaterThan(v) if v == "18"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_scalar_op_with_bool() {
        let args = json!({ "where": { "active": { "equals": true } } });
        let clauses = parse_where_filters(&args).unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "1"));
            }
            other => panic!("Expected Single, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_unknown_op_errors() {
        // Unknown operator name → hard error (not silently skipped), so a
        // typo'd or hallucinated operator can't return unfiltered results.
        let args = json!({ "where": { "field": { "unknown_op": "val" } } });
        let err = parse_where_filters(&args).unwrap_err().to_string();
        assert!(
            err.contains("unknown_op"),
            "should name the bad operator: {err}"
        );
    }

    // (A bare null field value is now rejected — see
    // `parse_where_null_value_is_rejected` below. It used to be a silent no-op.)

    #[test]
    fn parse_where_null_op_value_errors() {
        // A null value for a scalar operator is malformed → hard error,
        // rather than silently dropping the condition.
        let args = json!({ "where": { "field": { "equals": null } } });
        let err = parse_where_filters(&args).unwrap_err().to_string();
        assert!(err.contains("equals"), "should name the operator: {err}");
    }

    #[test]
    fn parse_where_no_where_key() {
        let args = json!({ "limit": 10 });
        let clauses = parse_where_filters(&args).unwrap();
        assert!(clauses.is_empty());
    }

    #[test]
    fn parse_where_non_object_where() {
        let args = json!({ "where": "not-an-object" });
        let clauses = parse_where_filters(&args).unwrap();
        assert!(clauses.is_empty());
    }

    // ── doc_to_json ────────────────────────────────────────────────────────

    #[test]
    fn doc_to_json_includes_all_fields() {
        let mut fields = HashMap::new();
        fields.insert("title".to_string(), json!("Hello"));
        fields.insert("count".to_string(), json!(42));
        let doc = Document {
            id: DocumentId::new("abc123"),
            fields: fields.into(),
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
            updated_at: Some("2024-06-01T00:00:00Z".to_string()),
        };
        let val = doc_to_json(&doc);
        assert_eq!(val["id"], "abc123");
        assert_eq!(val["title"], "Hello");
        assert_eq!(val["count"], 42);
        assert_eq!(val["created_at"], "2024-01-01T00:00:00Z");
        assert_eq!(val["updated_at"], "2024-06-01T00:00:00Z");
    }

    #[test]
    fn doc_to_json_without_timestamps() {
        let doc = Document {
            id: DocumentId::new("xyz"),
            fields: DocumentFields::new(),
            created_at: None,
            updated_at: None,
        };
        let val = doc_to_json(&doc);
        assert_eq!(val["id"], "xyz");
        assert!(val.get("created_at").is_none() || val["created_at"].is_null());
        assert!(val.get("updated_at").is_none() || val["updated_at"].is_null());
    }

    // ── parse_where_filters: fail-loud on silently-dropping shapes ─────────

    /// Regression: a bare array value used to hit `_ => {}` and drop the clause
    /// silently — which on `delete_many`/`update_many` widens to the whole
    /// collection. It must error instead.
    #[test]
    fn parse_where_bare_array_value_is_rejected() {
        let args = json!({ "where": { "status": ["draft", "review"] } });
        let err = parse_where_filters(&args).unwrap_err().to_string();
        assert!(err.contains("array value"), "got: {err}");
    }

    /// Regression: `in`/`not_in` with a non-array value used to `continue`
    /// (silently dropping the clause). It must error.
    #[test]
    fn parse_where_in_non_array_is_rejected() {
        let args = json!({ "where": { "status": { "in": "draft" } } });
        let err = parse_where_filters(&args).unwrap_err().to_string();
        assert!(err.contains("array value"), "got: {err}");
    }

    /// A null field value is rejected (use `exists`/`not_exists`), not dropped.
    #[test]
    fn parse_where_null_value_is_rejected() {
        let args = json!({ "where": { "status": null } });
        assert!(parse_where_filters(&args).is_err());
    }

    // ── extract_data_from_args: strict unknown-field rejection ────────────

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, crate::core::FieldType::Text).build()
    }

    #[test]
    fn extract_data_keeps_known_fields_and_skips_reserved() {
        let fields = vec![text_field("title"), text_field("body")];
        let args = json!({ "title": "Hi", "body": "x", "locale": "en", "extra_null": null });
        let data = extract_data_from_args(&args, &["locale"], &fields).unwrap();
        assert_eq!(data.get("title").and_then(Value::as_str), Some("Hi"));
        assert_eq!(data.get("body").and_then(Value::as_str), Some("x"));
        assert!(data.get("locale").is_none(), "reserved key excluded");
        assert!(data.get("extra_null").is_none(), "null dropped");
    }

    /// Regression: an unknown/misspelled field name must fail loudly rather than
    /// being silently dropped by the field-driven write pipeline.
    #[test]
    fn extract_data_rejects_unknown_field() {
        let fields = vec![text_field("title")];
        let args = json!({ "title": "Hi", "titel": "typo" });
        let err = extract_data_from_args(&args, &[], &fields)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field 'titel'"), "got: {err}");
    }

    /// Layout wrappers (Row/Collapsible/Tabs) are transparent — their sub-fields
    /// are valid top-level keys, so they must not be rejected.
    #[test]
    fn extract_data_accepts_row_sub_fields() {
        let row = FieldDefinition::builder("row", crate::core::FieldType::Row)
            .fields(vec![text_field("first"), text_field("last")])
            .build();
        let fields = vec![row];
        let args = json!({ "first": "a", "last": "b" });
        let data = extract_data_from_args(&args, &[], &fields).unwrap();
        assert_eq!(data.get("first").and_then(Value::as_str), Some("a"));
        assert_eq!(data.get("last").and_then(Value::as_str), Some("b"));
    }
}
