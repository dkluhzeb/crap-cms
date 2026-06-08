//! After-read hook pipeline: field-level + collection-level + global-registered.

use anyhow::Result;
use mlua::Lua;
use serde_json::Value as JsonValue;
use tracing::error;

use crate::{
    core::{Document, FieldDefinition, collection::Hooks, document::DocumentBuilder},
    hooks::lifecycle::{FieldHookEvent, HookEvent, context::HookContext, runner::FieldHooksCall},
};

use super::field_hooks::{has_any_field_hook, run_field_hooks_inner};
use super::runtime::{call_hook_ref, call_registered_hooks, get_hook_refs, has_registered_hooks};

pub struct AfterReadCtx<'a> {
    pub hooks: &'a Hooks,
    pub fields: &'a [FieldDefinition],
    pub collection: &'a str,
    pub operation: &'a str,
    /// Content locale for this read (nil when not locale-scoped).
    pub locale: Option<&'a str>,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
}

/// Inner implementation of `apply_after_read` — operates on a locked `&Lua`.
/// Runs field-level `after_read` hooks, then collection-level, then global registered.
/// On error: logs warning, returns original doc unmodified.
pub(crate) fn apply_after_read_inner(lua: &Lua, ctx: &AfterReadCtx, doc: Document) -> Document {
    let has_field_hooks = has_any_field_hook(ctx.fields, &FieldHookEvent::AfterRead);

    let has_collection_hooks = !ctx.hooks.after_read.is_empty();
    let has_registered = has_registered_hooks(lua, "after_read");

    if !has_field_hooks && !has_collection_hooks && !has_registered {
        return doc;
    }

    let doc_id = doc.id.to_string();
    let mut data = doc.fields.clone();
    data.insert("id".to_string(), JsonValue::String(doc_id.clone()));

    if let Some(ref ts) = doc.created_at {
        data.insert("created_at".to_string(), JsonValue::String(ts.clone()));
    }
    if let Some(ref ts) = doc.updated_at {
        data.insert("updated_at".to_string(), JsonValue::String(ts.clone()));
    }

    // Run field-level after_read hooks first
    if has_field_hooks
        && let Err(e) = run_field_hooks_inner(
            lua,
            &mut data,
            &FieldHooksCall {
                fields: ctx.fields,
                event: FieldHookEvent::AfterRead,
                collection: ctx.collection,
                operation: ctx.operation,
                id: Some(&doc_id),
                locale: ctx.locale,
            },
        )
    {
        error!(
            "field after_read hook error for {}: {:#}",
            ctx.collection, e
        );

        return doc;
    }

    let hook_ctx = HookContext::builder(ctx.collection, ctx.operation)
        .data(data)
        .locale(ctx.locale)
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();

    // Run collection-level + global registered hooks
    let hook_refs = get_hook_refs(ctx.hooks, HookEvent::AfterRead);
    let result = (|| -> Result<HookContext> {
        let mut context = hook_ctx;

        for hook_ref in hook_refs {
            context = call_hook_ref(lua, hook_ref, context)?;
        }

        context = call_registered_hooks(lua, HookEvent::AfterRead, context)?;

        Ok(context)
    })();

    match result {
        Ok(result_ctx) => {
            let mut fields = result_ctx.data;

            fields.remove("id");

            let created_at = fields
                .remove("created_at")
                .and_then(|v| v.as_str().map(std::string::ToString::to_string))
                .or(doc.created_at.clone());
            let updated_at = fields
                .remove("updated_at")
                .and_then(|v| v.as_str().map(std::string::ToString::to_string))
                .or(doc.updated_at.clone());

            DocumentBuilder::new(doc.id)
                .fields(fields)
                .created_at(created_at)
                .updated_at(updated_at)
                .build()
        }
        Err(e) => {
            error!("after_read hook error for {}: {:#}", ctx.collection, e);

            doc
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldType;
    use serde_json::json;

    #[test]
    fn apply_after_read_no_hooks_returns_unchanged() {
        let lua = mlua::Lua::new();
        lua.set_named_registry_value("_crap_event_hooks", lua.create_table().unwrap())
            .unwrap();
        let hooks = Hooks::default();
        let fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        let mut doc = Document::new("doc1".to_string());
        doc.fields.insert("title".to_string(), json!("Hello"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-02".to_string());

        let ctx = AfterReadCtx {
            hooks: &hooks,
            fields: &fields,
            collection: "posts",
            operation: "find",
            locale: None,
            user: None,
            ui_locale: None,
        };
        let result = apply_after_read_inner(&lua, &ctx, doc.clone());
        assert_eq!(result.id, "doc1");
        assert_eq!(result.get_str("title"), Some("Hello"));
    }
}
