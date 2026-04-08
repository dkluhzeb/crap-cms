//! Shared helper functions for the service layer.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    core::{Document, FieldDefinition, collection::Hooks},
    db::DbConnection,
    hooks::{HookContext, HookEvent},
    service::{AfterChangeInput, hooks::WriteHooks},
};

/// Build the hook data map from form data + structured join data.
/// Converts string values to JSON strings and merges in blocks/arrays/has-many.
pub(crate) fn build_hook_data(
    data: &HashMap<String, String>,
    join_data: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    let mut hook_data: HashMap<String, Value> = data
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    hook_data
}

/// Run after-change hooks and return the request-scoped context.
/// This pattern is repeated across create, update, unpublish, and global update.
pub(crate) fn run_after_change_hooks(
    write_hooks: &dyn WriteHooks,
    hooks: &Hooks,
    fields: &[FieldDefinition],
    doc: &Document,
    input: AfterChangeInput<'_>,
    tx: &dyn DbConnection,
) -> anyhow::Result<HashMap<String, Value>> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));
    let after_ctx = HookContext::builder(input.slug, input.operation)
        .data(after_data)
        .draft(input.is_draft)
        .locale(input.locale)
        .context(input.req_context)
        .user(input.user)
        .ui_locale(input.ui_locale)
        .build();
    let after_result =
        write_hooks.run_after_write(hooks, fields, HookEvent::AfterChange, after_ctx, tx)?;
    Ok(after_result.context)
}
