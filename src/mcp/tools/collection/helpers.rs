//! Shared helpers for collection CRUD tool implementations.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::{
    core::{
        CollectionDefinition, Document, DocumentFields, FieldDefinition,
        upload::write_shape_fields, writable_field_names,
    },
    db::{query, query::filter::decode_where_map},
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
///
/// # Errors
///
/// When `password` is present but not a string. It used to be coerced to
/// `None` and, because `password` is a reserved key, dropped from the data
/// too — so `{"password": 12345}` created an account with no password at all.
pub(in crate::mcp::tools) fn extract_auth_password(
    def: &CollectionDefinition,
    obj: &Value,
    empty_as_none: bool,
) -> Result<Option<String>> {
    if !def.is_auth_collection() {
        return Ok(None);
    }

    let Some(value) = obj.get("password") else {
        return Ok(None);
    };

    let Some(pw) = value.as_str() else {
        bail!("'password' must be a string");
    };

    if empty_as_none && pw.is_empty() {
        return Ok(None);
    }

    Ok(Some(pw.to_string()))
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

/// Decode the optional `where` tool argument through the canonical shared
/// grammar ([`decode_where_map`]) — scalar shorthand, operator objects, and
/// `or` groups, identical to gRPC and Lua. (The old MCP-local decoder
/// rejected `or` and silently dropped non-scalar `in` elements.)
pub(in crate::mcp::tools) fn parse_where_filters(args: &Value) -> Result<Vec<query::FilterClause>> {
    let Some(where_val) = args.get("where") else {
        return Ok(Vec::new());
    };

    // Never-silently-widen: a present-but-wrong-shaped
    // `where` must hard-error, not decay to zero filters — on
    // `delete_many`/`update_many` an empty filter means "every
    // document". The classic mistake is sending gRPC's JSON-*string*
    // spelling to the JSON-native MCP surface.
    let Some(where_obj) = where_val.as_object() else {
        bail!(
            "MCP where: must be a JSON object (e.g. {{\"status\": {{\"equals\": \"draft\"}}}}), \
             got {} — on this surface `where` is a real object, not the \
             JSON-encoded string the gRPC field uses",
            json_type_name(where_val)
        );
    };

    decode_where_map(where_obj).map_err(|e| anyhow!("MCP where: {e}"))
}

/// Human-readable JSON type name for error messages.
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
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
/// A key that is neither a `skip_key` nor a **writable** top-level field of the
/// collection is **rejected** (rather than silently dropped by the field-driven
/// write pipeline) so a hallucinated/misspelled field name on this AI-driven
/// surface fails loudly. Layout wrappers (Row/Collapsible/Tabs) are transparent,
/// so their sub-fields are the valid top-level keys; a virtual `Join` field is
/// not writable, so a value sent for one is rejected like a typo instead of
/// being dropped at persist.
///
/// # Errors
///
/// Returns an error naming any key that is not a `skip_key` and not a writable
/// field of the collection.
pub(in crate::mcp::tools) fn extract_data_from_args(
    args: &Value,
    skip_keys: &[&str],
    fields: &[FieldDefinition],
) -> Result<DocumentFields> {
    let Some(obj) = args.as_object() else {
        return Ok(DocumentFields::new());
    };

    let known = writable_field_names(fields);

    let mut data = DocumentFields::new();

    for (k, v) in obj {
        if skip_keys.contains(&k.as_str()) {
            continue;
        }

        if !known.contains(k.as_str()) {
            bail!("unknown field '{k}' for this collection");
        }

        // `null` is KEPT, not dropped: a present null clears the field, the
        // same contract gRPC and Lua have. Dropping it made a clear silently
        // no-op — and made removing a translation (documented as writing that
        // locale's fields as null) impossible over MCP.
        data.insert(k.clone(), v.clone());
    }

    Ok(data)
}

/// [`extract_data_from_args`] against a collection's write shape — the keys
/// its write-tool schema advertises. An upload collection's server-derived
/// columns (`filename`, `url`, `mime_type`, …) are no field a caller may send,
/// so a value for one is rejected like any unknown key instead of being
/// stripped later.
///
/// # Errors
///
/// Returns an error naming any key that is not a `skip_key` and not a field of
/// the collection's write shape.
pub(in crate::mcp::tools) fn extract_collection_data(
    args: &Value,
    skip_keys: &[&str],
    def: &CollectionDefinition,
) -> Result<DocumentFields> {
    extract_data_from_args(args, skip_keys, &write_shape_fields(def))
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
        core::{
            DocumentFields, DocumentId, JoinConfig, Slug, collection::Auth, document::Document,
            upload::CollectionUpload,
        },
        db::query,
    };

    /// The shared password extractor preserves the intended
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
            extract_auth_password(&auth_def, &empty, false).unwrap(),
            Some(String::new()),
            "create: empty flows through to the policy validator"
        );
        assert_eq!(
            extract_auth_password(&auth_def, &empty, true).unwrap(),
            None,
            "update: empty means no change"
        );

        let real = json!({ "password": "secret" });
        assert_eq!(
            extract_auth_password(&auth_def, &real, true).unwrap(),
            Some("secret".to_string())
        );

        let plain_def = CollectionDefinition::new("posts");
        assert_eq!(
            extract_auth_password(&plain_def, &real, false).unwrap(),
            None,
            "non-auth collection: password is ordinary field data"
        );
    }

    /// A non-string password used to coerce to `None` — and because
    /// `password` is a reserved key it was stripped from the data too, so
    /// `{"password": 12345}` created an account with no password at all.
    #[test]
    fn extract_auth_password_rejects_a_non_string() {
        let mut auth_def = CollectionDefinition::new("users");
        auth_def.auth = Some(Auth::new(true));

        for value in [json!(12345), json!(true), json!(["a"]), json!(null)] {
            let args = json!({ "password": value });
            let err = extract_auth_password(&auth_def, &args, false)
                .expect_err("a non-string password must be rejected");
            assert!(err.to_string().contains("must be a string"));
        }
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
                // Unified grammar: booleans stringify as true/false; the SQL
                // edge coerces per column type (true ↔ 1 on Checkbox).
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "true"));
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
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "false"));
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
                assert!(matches!(&f.op, query::FilterOp::Equals(v) if v == "true"));
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
        assert!(
            err.contains("string, number, or boolean"),
            "unified scalar-value error: {err}"
        );
    }

    #[test]
    fn parse_where_no_where_key() {
        let args = json!({ "limit": 10 });
        let clauses = parse_where_filters(&args).unwrap();
        assert!(clauses.is_empty());
    }

    #[test]
    fn parse_where_non_object_where() {
        // Never-silently-widen: a present-but-non-object
        // `where` MUST hard-error, not decay to zero filters — an empty
        // filter on a bulk op means "every document". The classic mistake
        // is the gRPC JSON-string spelling on the object-native MCP surface.
        for bad in [json!("not-an-object"), json!(["x"]), json!(42)] {
            let err = parse_where_filters(&json!({ "where": bad }))
                .unwrap_err()
                .to_string();
            assert!(err.contains("JSON object"), "must name the shape: {err}");
        }

        // Absent `where` is still the honest empty-filter case.
        assert!(parse_where_filters(&json!({})).unwrap().is_empty());
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
        assert!(err.contains("cannot be an array"), "got: {err}");
    }

    /// Regression: `in`/`not_in` with a non-array value used to `continue`
    /// (silently dropping the clause). It must error.
    #[test]
    fn parse_where_in_non_array_is_rejected() {
        let args = json!({ "where": { "status": { "in": "draft" } } });
        let err = parse_where_filters(&args).unwrap_err().to_string();
        assert!(err.contains("requires an array"), "got: {err}");
    }

    /// A null field value is rejected (use `exists`/`not_exists`), not dropped.
    #[test]
    fn parse_where_null_value_is_rejected() {
        let args = json!({ "where": { "status": null } });
        assert!(parse_where_filters(&args).is_err());
    }

    /// A present `null` reaches the write path (it clears the column), while
    /// a reserved key is still skipped — parity with gRPC and Lua.
    #[test]
    fn extract_data_keeps_present_nulls() {
        let fields = vec![text_field("title"), text_field("subtitle")];
        let args = json!({ "title": "t", "subtitle": null, "locale": null });

        let data = extract_data_from_args(&args, &["locale"], &fields).unwrap();
        assert_eq!(data.get("subtitle"), Some(&Value::Null));
        assert!(!data.contains_key("locale"), "reserved keys stay skipped");
    }

    // ── extract_data_from_args: strict unknown-field rejection ────────────

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, crate::core::FieldType::Text).build()
    }

    #[test]
    fn extract_data_keeps_known_fields_and_skips_reserved() {
        let fields = vec![text_field("title"), text_field("body")];
        let args = json!({ "title": "Hi", "body": "x", "locale": "en" });
        let data = extract_data_from_args(&args, &["locale"], &fields).unwrap();
        assert_eq!(data.get("title").and_then(Value::as_str), Some("Hi"));
        assert_eq!(data.get("body").and_then(Value::as_str), Some("x"));
        assert!(data.get("locale").is_none(), "reserved key excluded");

        // An unknown key is rejected whatever its value — a null no longer
        // buys silence, since a present null is now meaningful (it clears).
        let args = json!({ "title": "Hi", "extra_null": null });
        assert!(extract_data_from_args(&args, &["locale"], &fields).is_err());
    }

    /// Regression: the strict extractor accepted an upload collection's
    /// server-derived columns (`filename`, `url`, …), which the service then
    /// stripped without a word. The collection extractor walks the write shape,
    /// so they are rejected like any key the schema does not advertise.
    #[test]
    fn collection_extractor_rejects_derived_upload_columns() {
        let mut def = CollectionDefinition::new("media");
        def.fields = vec![text_field("filename"), text_field("url"), text_field("alt")];
        def.upload = Some(CollectionUpload::new());

        let data = extract_collection_data(&json!({ "alt": "a" }), &[], &def).unwrap();
        assert_eq!(data.get("alt").and_then(Value::as_str), Some("a"));

        for derived in ["filename", "url"] {
            let args: Map<String, Value> =
                [(derived.to_string(), json!("x"))].into_iter().collect();
            let err = extract_collection_data(&Value::Object(args), &[], &def)
                .unwrap_err()
                .to_string();
            assert!(err.contains(derived), "{derived}: {err}");
        }
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

    /// Regression: a `Join` field is virtual — no column, no join table — so a
    /// value sent for it can never be stored. It passed the known-field check
    /// (the flatten classifies `Join` as a leaf) and was silently discarded at
    /// persist; it must be reported like any other unwritable key.
    #[test]
    fn extract_data_rejects_a_join_field_key() {
        let mut join = FieldDefinition::builder("comments", crate::core::FieldType::Join).build();
        join.join = Some(JoinConfig {
            collection: Slug::new("comments"),
            on: "post".to_string(),
        });
        let fields = vec![text_field("title"), join];

        let err = extract_data_from_args(&json!({ "comments": "x" }), &[], &fields)
            .expect_err("a Join key is not writable");
        assert!(
            err.to_string().contains("unknown field 'comments'"),
            "{err}"
        );

        // A real typo still errors, and a writable field still passes.
        assert!(extract_data_from_args(&json!({ "titel": "x" }), &[], &fields).is_err());
        let data = extract_data_from_args(&json!({ "title": "x" }), &[], &fields).unwrap();
        assert_eq!(data.get("title").and_then(Value::as_str), Some("x"));
    }

    /// A nested composite still passes: only the virtual `Join` type is
    /// unwritable, so a group's object value reaches the write pipeline.
    #[test]
    fn extract_data_accepts_a_nested_group_value() {
        let group = FieldDefinition::builder("seo", crate::core::FieldType::Group)
            .fields(vec![text_field("meta_title")])
            .build();
        let fields = vec![group];

        let data =
            extract_data_from_args(&json!({ "seo": { "meta_title": "t" } }), &[], &fields).unwrap();
        assert_eq!(
            data.get("seo").and_then(|v| v.get("meta_title")),
            Some(&json!("t"))
        );
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
