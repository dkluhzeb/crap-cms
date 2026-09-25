//! Top-level validation entry point and shared value-shape helpers.

use mlua::Lua;
use serde_json::Value;

use crate::core::{
    DocumentFields, FieldDefinition, Registry, flatten_group_fields, nul_character_errors,
    validate::ValidationError,
};

use super::{ValidationCtx, recursive::ValidationWalker, stored::StoredDocument};

/// Validate a write's field data — the one entry point every write path uses
/// (`HookRunner::validate_fields` and the in-VM Lua CRUD write hooks).
///
/// The registry is a required argument, not an optional context slot, so no
/// write path can reach validation without the richtext node definitions its
/// node-attr checks read.
pub(crate) fn validate_write_fields(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    ctx: &ValidationCtx,
    registry: &Registry,
) -> Result<(), ValidationError> {
    let ctx = ValidationCtx {
        registry: Some(registry),
        ..*ctx
    };

    validate_fields_inner(lua, fields, data, &ctx)
}

/// Schema walk + completeness gate over an already-assembled context. Write
/// paths go through [`validate_write_fields`]; this stays module-internal so
/// the checks' own tests can drive it with a hand-built context.
pub(in crate::hooks::lifecycle::validation) fn validate_fields_inner(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    ctx: &ValidationCtx,
) -> Result<(), ValidationError> {
    // Validation is column-oriented (unique constraints, per-column required,
    // locale columns), so the schema walk runs over a **flat** `group__sub` view
    // — the canonical nested `data` is flattened here (idempotent). The original
    // nested `data` is still threaded through as `document` so user-defined
    // predicates (`required_when`, custom `validate`) see the canonical nested
    // shape, matching field hooks and field access.
    let flat = flatten_group_fields(data, fields);

    // A NUL anywhere in the write is refused before any check that queries the
    // database with the value (a unique check would hand Postgres a string it
    // cannot even compare).
    let nul_errors = nul_character_errors(data, fields, &[]);

    if !nul_errors.is_empty() {
        return Err(ValidationError::new(nul_errors));
    }

    // What the edited document already holds — read only if a check asks.
    let stored = StoredDocument::new(ctx, fields);

    let mut errors = Vec::new();
    ValidationWalker::new(lua, &flat, data, &stored).walk(fields, "", false, &mut errors);

    // Document-level: localized required fields must be complete across their
    // `required_locales` (reads the existing row for non-write locales).
    super::check_localized_completeness(lua, fields, &flat, data, ctx, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::new(errors))
    }
}

/// Returns true when `value` represents an absent or blank field — `None`,
/// `Null`, or an empty string. Used identically by every validator that must
/// distinguish empty input from non-empty input (e.g. to skip format checks
/// on empty fields).
pub(in crate::hooks::lifecycle::validation) fn is_empty_value(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    #[cfg(feature = "sqlite")]
    use crate::{core::FieldType, db::InMemoryConn};

    /// Regression: a NUL was refused only in top-level text/textarea/email
    /// columns, so one inside an array row or a JSON field was stored — and on
    /// Postgres, whose `::jsonb` cast rejects the `\u0000` escape, it broke
    /// every row-path filter on the collection. Every depth is checked, and the
    /// NUL error is returned alone: the unique check would otherwise query the
    /// database with the value.
    #[cfg(feature = "sqlite")]
    #[test]
    fn a_nul_anywhere_in_the_write_is_rejected() {
        let lua = Lua::new();
        let conn = InMemoryConn::open();
        conn.setup("CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT, meta TEXT)");

        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .unique(true)
                .required(true)
                .build(),
            FieldDefinition::builder("meta", FieldType::Json).build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                ])
                .build(),
        ];
        let data: DocumentFields = [
            ("slug".to_string(), json!("a\0b")),
            ("meta".to_string(), json!("{\"k\": \"\\u0000\"}")),
            ("items".to_string(), json!([{ "label": "\0" }])),
        ]
        .into_iter()
        .collect();
        let ctx = ValidationCtx::builder(&conn, "test").draft(true).build();

        let err = validate_fields_inner(&lua, &fields, &data, &ctx).unwrap_err();
        let mut keys: Vec<&str> = err.errors.iter().map(|e| e.field.as_str()).collect();
        keys.sort_unstable();

        assert_eq!(keys, vec!["items[0][label]", "meta", "slug"]);
        assert!(
            err.errors
                .iter()
                .all(|e| e.key.as_deref() == Some("validation.nul_character"))
        );
    }

    #[test]
    fn absent_null_and_empty_string_are_empty() {
        assert!(is_empty_value(None));
        assert!(is_empty_value(Some(&Value::Null)));
        assert!(is_empty_value(Some(&json!(""))));
    }

    #[test]
    fn present_values_are_not_empty() {
        assert!(!is_empty_value(Some(&json!("x"))));
        // Non-string types are never "empty" — including the falsy/zero/blank
        // shapes, so a `0`, `false`, `[]` or `{}` still counts as provided.
        assert!(!is_empty_value(Some(&json!(0))));
        assert!(!is_empty_value(Some(&json!(false))));
        assert!(!is_empty_value(Some(&json!([]))));
        assert!(!is_empty_value(Some(&json!({}))));
        assert!(!is_empty_value(Some(&json!(" "))));
    }
}
