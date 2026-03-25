//! Field validation logic: required checks, unique checks, date format, custom Lua validators,
//! and display condition evaluation.

mod checks;
mod custom;
mod recursive;
pub(crate) mod richtext_attrs;
mod sub_fields;

// Re-export public API
pub use checks::evaluate_condition_table;

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{FieldDefinition, registry::Registry, validate::ValidationError},
    db::{DbConnection, LocaleContext},
};

/// Context for field validation, bundling database and request parameters.
pub struct ValidationCtx<'a> {
    pub conn: &'a dyn DbConnection,
    pub table: &'a str,
    pub exclude_id: Option<&'a str>,
    pub is_draft: bool,
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Registry for looking up richtext node definitions during node attr validation.
    pub registry: Option<&'a Registry>,
}

impl<'a> ValidationCtx<'a> {
    /// Create a builder with the required connection and table name.
    pub fn builder(conn: &'a dyn DbConnection, table: &'a str) -> ValidationCtxBuilder<'a> {
        ValidationCtxBuilder::new(conn, table)
    }
}

/// Builder for [`ValidationCtx`]. Created via [`ValidationCtx::builder`].
pub struct ValidationCtxBuilder<'a> {
    conn: &'a dyn DbConnection,
    table: &'a str,
    exclude_id: Option<&'a str>,
    is_draft: bool,
    locale_ctx: Option<&'a LocaleContext>,
    registry: Option<&'a Registry>,
}

impl<'a> ValidationCtxBuilder<'a> {
    fn new(conn: &'a dyn DbConnection, table: &'a str) -> Self {
        Self {
            conn,
            table,
            exclude_id: None,
            is_draft: false,
            locale_ctx: None,
            registry: None,
        }
    }

    pub fn exclude_id(mut self, exclude_id: Option<&'a str>) -> Self {
        self.exclude_id = exclude_id;
        self
    }

    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn registry(mut self, registry: &'a Registry) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn build(self) -> ValidationCtx<'a> {
        ValidationCtx {
            conn: self.conn,
            table: self.table,
            exclude_id: self.exclude_id,
            is_draft: self.is_draft,
            locale_ctx: self.locale_ctx,
            registry: self.registry,
        }
    }
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
    recursive::validate_fields_recursive(lua, fields, data, ctx, "", false, &mut errors);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::new(errors))
    }
}
