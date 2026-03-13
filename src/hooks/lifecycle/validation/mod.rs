//! Field validation logic: required checks, unique checks, date format, custom Lua validators,
//! and display condition evaluation.

mod checks;
mod custom;
mod recursive;
mod sub_fields;

// Re-export public API
pub use checks::evaluate_condition_table;

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{field::FieldDefinition, validate::ValidationError},
    db::query::LocaleContext,
};

/// Context for field validation, bundling database and request parameters.
pub struct ValidationCtx<'a> {
    pub conn: &'a rusqlite::Connection,
    pub table: &'a str,
    pub exclude_id: Option<&'a str>,
    pub is_draft: bool,
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// Inner implementation of `validate_fields` — operates on a locked `&Lua`.
/// Used by both `HookRunner::validate_fields` and Lua CRUD closures.
pub(crate) fn validate_fields_inner(
    lua: &mlua::Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, Value>,
    ctx: &ValidationCtx,
) -> Result<(), ValidationError> {
    let mut errors = Vec::new();
    recursive::validate_fields_recursive(
        lua,
        fields,
        data,
        ctx.conn,
        ctx.table,
        ctx.exclude_id,
        ctx.is_draft,
        "",
        ctx.locale_ctx,
        false,
        &mut errors,
    );

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::new(errors))
    }
}
