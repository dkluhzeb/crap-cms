//! Field validation logic: required checks, unique checks, date format, custom Lua validators,
//! and display condition evaluation.

mod checks;
mod custom;
mod recursive;
mod sub_fields;

// Re-export public API
pub use checks::evaluate_condition_table;

use std::collections::HashMap;

use crate::core::field::FieldDefinition;
use crate::core::validate::ValidationError;
use crate::db::query::LocaleContext;

/// Inner implementation of `validate_fields` — operates on a locked `&Lua`.
/// Used by both `HookRunner::validate_fields` and Lua CRUD closures.
pub(crate) fn validate_fields_inner(
    lua: &mlua::Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
    locale_ctx: Option<&LocaleContext>,
) -> Result<(), ValidationError> {
    let mut errors = Vec::new();
    recursive::validate_fields_recursive(
        lua,
        fields,
        data,
        conn,
        table,
        exclude_id,
        is_draft,
        "",
        locale_ctx,
        false,
        &mut errors,
    );
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::new(errors))
    }
}
