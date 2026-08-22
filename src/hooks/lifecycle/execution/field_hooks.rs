//! Field-level hook execution.
//!
//! Field hooks run per-field and receive `(value, context)`, returning the new
//! value. The `FieldHookWalker` recurses over the canonical **nested** document
//! shape. Every field that carries a value — scalar, Group, Array, Blocks —
//! fires its own hook on that value; Group/Array/Blocks additionally recurse
//! into their nested sub-fields. Row/Collapsible/Tabs are transparent layout
//! wrappers with no value of their own, so they only pass through to their
//! children (lifecycle hooks on them are rejected at parse time). A hook's
//! `ctx.data` is its nearest scope (the group object / array row / document).

use std::time::Instant;

use anyhow::{Result, anyhow};
use mlua::{Lua, LuaSerdeExt as _, Value};
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::{
    core::{
        BLOCK_TYPE_KEY, DocumentFields, FieldDefinition, FieldType, HookRef, any_field,
        field::FieldHooks,
    },
    hooks::{
        lifecycle::{
            FieldHookContext, FieldHookEvent, UiLocaleContext, UserContext, runner::FieldHooksCall,
        },
        lua_api,
    },
};

use super::resolve_hook_function;

/// Check if any fields (including nested sub-fields) have hooks registered
/// for the given field-level event.
pub(crate) fn has_field_hooks_for_event(
    fields: &[FieldDefinition],
    event: &FieldHookEvent,
) -> bool {
    has_any_field_hook(fields, event)
}

/// Shared implementation for `run_field_hooks` and `run_field_hooks_with_conn`.
/// Caller is responsible for locking the Lua VM and (optionally) setting `TxContext`.
pub(crate) fn run_field_hooks_inner(
    lua: &Lua,
    data: &mut DocumentFields,
    call: &FieldHooksCall<'_>,
) -> Result<()> {
    // Snapshot the full document up front so every hook — including those on
    // sub-fields inside array/blocks rows, where `data` narrows to the row —
    // can reach the whole document via `ctx.document`. We're already gated on
    // "this collection has field hooks for this event" (see the runner), so
    // the clone only happens on writes/reads that actually run field hooks.
    let document = data.clone();

    FieldHookWalker {
        lua,
        call,
        document: &document,
    }
    .walk(data, call.fields)
}

/// Iterator state for the recursive field-hook walk. Bundles the per-walk
/// invariants (Lua VM, call descriptor, full-document snapshot) so the
/// recursive helpers stay at ≤ 3 args + receiver instead of 6+ positional
/// args.
struct FieldHookWalker<'a> {
    lua: &'a Lua,
    call: &'a FieldHooksCall<'a>,
    /// Full-document snapshot exposed to every hook as `ctx.document`,
    /// unchanged as the walk descends into array/blocks rows.
    document: &'a DocumentFields,
}

impl FieldHookWalker<'_> {
    /// Recursive field-hook execution over the canonical **nested** document
    /// shape — the same walk at the top level and inside array/blocks rows
    /// (group data is a nested object at every level). Group/Array/Blocks fire
    /// the field's own hook on its value then recurse into their sub-fields;
    /// Row/Collapsible/Tabs are transparent (pass-through only); every other
    /// field fires its own hook.
    fn walk(&self, data: &mut DocumentFields, fields: &[FieldDefinition]) -> Result<()> {
        for field in fields {
            match field.field_type {
                FieldType::Group => {
                    // A hook on the group field itself fires on the whole nested
                    // object (parity with Array/Blocks); sub-field hooks then
                    // fire within the possibly hook-updated object.
                    self.run_single(data, field)?;
                    self.walk_group(data, field)?;
                }

                FieldType::Row | FieldType::Collapsible => {
                    self.walk(data, &field.fields)?;
                }

                FieldType::Tabs => {
                    for tab in &field.tabs {
                        self.walk(data, &tab.fields)?;
                    }
                }

                FieldType::Array => {
                    // A hook on the array field itself fires on the whole value.
                    self.run_single(data, field)?;
                    // Hooks on the array's sub-fields fire per row.
                    self.walk_array_rows(data, field)?;
                }

                FieldType::Blocks => {
                    self.run_single(data, field)?;
                    self.walk_blocks_rows(data, field)?;
                }

                _ => {
                    self.run_single(data, field)?;
                }
            }
        }

        Ok(())
    }

    /// Run sub-field hooks within a `Group`'s nested object, writing the
    /// mutated object back. An absent group is treated as empty so sub-field
    /// hooks still run (e.g. to auto-generate a value); the object is written
    /// back only when non-empty, so an untouched absent group isn't
    /// materialized.
    fn walk_group(&self, data: &mut DocumentFields, field: &FieldDefinition) -> Result<()> {
        let mut group_data: DocumentFields = match data.get(&field.name) {
            Some(JsonValue::Object(obj)) => {
                obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            }
            _ => DocumentFields::new(),
        };

        self.walk(&mut group_data, &field.fields)?;

        if !group_data.is_empty() {
            data.insert(
                field.name.clone(),
                JsonValue::Object(
                    group_data
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                ),
            );
        }

        Ok(())
    }

    /// Run sub-field hooks on every row of an `Array` field, writing each
    /// mutated row back. No-op unless a sub-field actually has a hook for this
    /// event.
    fn walk_array_rows(&self, data: &mut DocumentFields, field: &FieldDefinition) -> Result<()> {
        if !has_any_field_hook(&field.fields, &self.call.event) {
            return Ok(());
        }

        let Some(JsonValue::Array(rows)) = data.get(&field.name) else {
            return Ok(());
        };
        let mut rows = rows.clone();

        for row in &mut rows {
            let Some(obj) = row.as_object() else {
                continue;
            };
            let mut row_data: DocumentFields =
                obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

            self.walk(&mut row_data, &field.fields)?;

            *row = JsonValue::Object(
                row_data
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
        }

        data.insert(field.name.clone(), JsonValue::Array(rows));
        Ok(())
    }

    /// Run sub-field hooks on every row of a `Blocks` field, resolving each
    /// row's `_block_type` to the matching block's sub-fields and preserving
    /// `_block_type` on write-back.
    fn walk_blocks_rows(&self, data: &mut DocumentFields, field: &FieldDefinition) -> Result<()> {
        if !field
            .blocks
            .iter()
            .any(|b| has_any_field_hook(&b.fields, &self.call.event))
        {
            return Ok(());
        }

        let Some(JsonValue::Array(rows)) = data.get(&field.name) else {
            return Ok(());
        };
        let mut rows = rows.clone();

        for row in &mut rows {
            let Some(obj) = row.as_object() else {
                continue;
            };
            let block_type = obj
                .get(BLOCK_TYPE_KEY)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let Some(block) = field.blocks.iter().find(|b| b.block_type == block_type) else {
                continue;
            };

            // Cloning the whole row keeps `_block_type` (and any non-hook keys)
            // intact on write-back; the walk only touches sub-fields with hooks.
            let mut row_data: DocumentFields =
                obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

            self.walk(&mut row_data, &block.fields)?;

            *row = JsonValue::Object(
                row_data
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
        }

        data.insert(field.name.clone(), JsonValue::Array(rows));
        Ok(())
    }

    /// Run hooks for a single (non-container) field. The field is addressed by
    /// bare name within the current (nested) level — its `ctx.data` is that
    /// level, matching `FieldHookContext`'s "nearest scope" contract.
    fn run_single(&self, data: &mut DocumentFields, field: &FieldDefinition) -> Result<()> {
        let hook_refs = get_field_hook_refs(&field.hooks, &self.call.event);

        if hook_refs.is_empty() {
            return Ok(());
        }

        let data_key = field.name.clone();

        let was_present = data.contains_key(&data_key);
        let value = data.get(&data_key).cloned().unwrap_or(JsonValue::Null);

        let mut current = value;
        let timing = tracing::enabled!(tracing::Level::DEBUG);
        let start = if timing { Some(Instant::now()) } else { None };

        let meta = FieldHookMeta {
            collection: self.call.collection,
            operation: self.call.operation,
            id: self.call.id,
            locale: self.call.locale,
        };

        for hook_ref in hook_refs {
            current = call_field_hook_ref(
                self.lua,
                hook_ref,
                &current,
                &data_key,
                &meta,
                data,
                self.document,
            )?;
        }

        if let Some(start) = start {
            debug!(
                "{}.{data_key}: {} field hook(s) in {:.2}ms",
                self.call.collection,
                hook_refs.len(),
                start.elapsed().as_secs_f64() * 1000.0,
            );
        }

        // Only write back if the field was already in the data, or the hook
        // produced a non-null value (e.g. auto_slug generating a slug on create).
        // Without this, absent fields on partial updates get coerced to Null,
        // which breaks the "skip required check for absent fields" logic.
        if was_present || !current.is_null() {
            data.insert(data_key, current);
        }

        Ok(())
    }
}

/// Recursively check if any field (including nested Group/Row/Collapsible/Tabs
/// children) has hooks registered for the given event.
pub(super) fn has_any_field_hook(fields: &[FieldDefinition], event: &FieldHookEvent) -> bool {
    any_field(fields, &|f| {
        !get_field_hook_refs(&f.hooks, event).is_empty()
    })
}

/// Get the list of field hook references for a given event.
pub(crate) fn get_field_hook_refs<'a>(
    hooks: &'a FieldHooks,
    event: &FieldHookEvent,
) -> &'a [HookRef] {
    match event {
        FieldHookEvent::BeforeValidate => &hooks.before_validate,
        FieldHookEvent::BeforeChange => &hooks.before_change,
        FieldHookEvent::AfterChange => &hooks.after_change,
        FieldHookEvent::AfterRead => &hooks.after_read,
    }
}

/// Call-level metadata shared by every field hook in a single walk. Bundled
/// so [`call_field_hook_ref`] stays within the argument-count budget.
pub(crate) struct FieldHookMeta<'a> {
    pub collection: &'a str,
    pub operation: &'a str,
    pub id: Option<&'a str>,
    pub locale: Option<&'a str>,
}

/// Resolve a hook reference and call it as a field hook.
/// Field hooks receive `(value, context)` and return the new value.
pub(crate) fn call_field_hook_ref(
    lua: &Lua,
    hook: &HookRef,
    value: &JsonValue,
    field_name: &str,
    meta: &FieldHookMeta<'_>,
    data: &DocumentFields,
    document: &DocumentFields,
) -> Result<JsonValue> {
    let hook_ref = hook.reference();
    let func = resolve_hook_function(lua, hook_ref)?;

    // Convert the field value to Lua
    let lua_value = lua_api::json_to_lua(lua, value)?;

    // Build context table from a typed Rust struct so the Lua shape is
    // the single source of truth (see
    // `hooks::lifecycle::FieldHookContext`).
    let user_ctx_ref = lua.app_data_ref::<UserContext>();
    let locale_ctx_ref = lua.app_data_ref::<UiLocaleContext>();
    let ctx = FieldHookContext {
        field_name,
        collection: meta.collection,
        operation: meta.operation,
        id: meta.id,
        locale: meta.locale,
        data,
        document,
        user: user_ctx_ref.as_ref().and_then(|c| c.0.as_ref()),
        ui_locale: locale_ctx_ref.as_ref().and_then(|c| c.0.as_deref()),
        options: hook.options(),
    };
    let ctx_table = lua.to_value(&ctx)?;

    // Call: new_value = hook(value, context)
    let result: Value = func.call((lua_value, ctx_table))?;

    // Convert result back to JSON
    lua_api::lua_to_json(&result)
        .map_err(|e| anyhow!("Field hook '{hook_ref}' returned invalid value: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldTab;
    use crate::core::{FieldHooks, FieldType};
    use serde_json::json;

    /// Regression: a `before_change` hook on a **group sub-field** must fire when
    /// the group arrives as a nested object (the canonical write shape). Before
    /// the nested-walker unification, the top-level walk looked up the flat key
    /// `seo__title` and silently skipped the hook for nested input.
    #[test]
    fn field_hook_runs_on_nested_group_subfield() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.upper"] = function(value, ctx)

                if type(value) == "string" then return value:upper() end

                return value
            end
        "#,
        )
        .exec()
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text)
                        .hooks(FieldHooks {
                            before_change: vec![HookRef::new("hooks.upper")],
                            ..Default::default()
                        })
                        .build(),
                ])
                .build(),
        ];

        let mut data: DocumentFields = [("seo".to_string(), json!({ "title": "hi" }))]
            .into_iter()
            .collect();

        run_field_hooks_inner(
            &lua,
            &mut data,
            &FieldHooksCall {
                fields: &fields,
                event: FieldHookEvent::BeforeChange,
                collection: "posts",
                operation: "create",
                id: None,
                locale: None,
            },
        )
        .unwrap();

        assert_eq!(
            data.get("seo")
                .and_then(|s| s.get("title"))
                .and_then(|v| v.as_str()),
            Some("HI"),
            "before_change hook must transform a nested group sub-field"
        );
    }

    /// Regression: a hook on a Group field ITSELF (not a sub-field) must fire on
    /// the whole nested object — parity with Array/Blocks. Before the container
    /// harmonization the Group arm only descended into sub-fields and silently
    /// skipped the group's own hook (the VM was acquired but the hook never ran).
    #[test]
    fn field_hook_runs_on_group_itself() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.stamp"] = function(value, ctx)

                if type(value) == "table" then
                    value.stamped = "yes"
                    return value
                end

                return value
            end
        "#,
        )
        .exec()
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .hooks(FieldHooks {
                    before_change: vec![HookRef::new("hooks.stamp")],
                    ..Default::default()
                })
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        let mut data: DocumentFields = [("meta".to_string(), json!({ "title": "hi" }))]
            .into_iter()
            .collect();

        run_field_hooks_inner(
            &lua,
            &mut data,
            &FieldHooksCall {
                fields: &fields,
                event: FieldHookEvent::BeforeChange,
                collection: "posts",
                operation: "create",
                id: None,
                locale: None,
            },
        )
        .unwrap();

        assert_eq!(
            data.get("meta")
                .and_then(|m| m.get("stamped"))
                .and_then(|v| v.as_str()),
            Some("yes"),
            "a before_change hook on the group field itself must fire on the whole object"
        );
        assert_eq!(
            data.get("meta")
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str()),
            Some("hi"),
            "sub-field values must be preserved through the group-level hook"
        );
    }

    #[test]
    fn field_hook_receives_value_and_context() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.upper"] = function(value, context)

                if type(value) == "string" then

                    return value:upper()
                end

                return value
            end
        "#,
        )
        .exec()
        .unwrap();

        let data: DocumentFields = [("title".to_string(), json!("hello"))]
            .into_iter()
            .collect();

        let result = call_field_hook_ref(
            &lua,
            &HookRef::new("hooks.upper"),
            &json!("hello"),
            "title",
            &FieldHookMeta {
                collection: "posts",
                operation: "create",
                id: None,
                locale: None,
            },
            &data,
            &data,
        )
        .unwrap();

        assert_eq!(result, json!("HELLO"));
    }

    #[test]
    fn field_hook_nil_value_does_not_crash() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.trim"] = function(value, context)

                if type(value) == "string" then

                    return value:match("^%s*(.-)%s*$")
                end

                return value
            end
        "#,
        )
        .exec()
        .unwrap();

        let data = DocumentFields::new();

        let result = call_field_hook_ref(
            &lua,
            &HookRef::new("hooks.trim"),
            &JsonValue::Null,
            "title",
            &FieldHookMeta {
                collection: "posts",
                operation: "update",
                id: None,
                locale: None,
            },
            &data,
            &data,
        )
        .unwrap();

        assert_eq!(result, JsonValue::Null);
    }

    #[test]
    fn field_hook_absent_field_not_injected_as_null() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.noop"] = function(value, context)

                return value
            end
        "#,
        )
        .exec()
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .hooks(FieldHooks {
                    before_validate: vec![HookRef::new("hooks.noop")],
                    ..Default::default()
                })
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("content".to_string(), json!("updated"));

        run_field_hooks_inner(
            &lua,
            &mut data,
            &FieldHooksCall {
                fields: &fields,
                event: FieldHookEvent::BeforeValidate,
                collection: "posts",
                operation: "update",
                id: None,
                locale: None,
            },
        )
        .unwrap();

        assert!(
            !data.contains_key("title"),
            "absent field should not be injected into data by field hooks"
        );
        assert_eq!(data.get("content"), Some(&json!("updated")));
    }

    #[test]
    fn field_hook_absent_field_inserted_when_hook_produces_value() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.default_val"] = function(value, context)

                if value == nil then

                    return "generated"
                end

                return value
            end
        "#,
        )
        .exec()
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .hooks(FieldHooks {
                    before_validate: vec![HookRef::new("hooks.default_val")],
                    ..Default::default()
                })
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));

        run_field_hooks_inner(
            &lua,
            &mut data,
            &FieldHooksCall {
                fields: &fields,
                event: FieldHookEvent::BeforeValidate,
                collection: "posts",
                operation: "create",
                id: None,
                locale: None,
            },
        )
        .unwrap();

        assert_eq!(data.get("slug"), Some(&json!("generated")));
    }

    #[test]
    fn field_hook_context_has_data_and_metadata() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.inspect_ctx"] = function(value, context)

                return context.collection .. ":" .. context.field_name .. ":" .. context.operation
            end
        "#,
        )
        .exec()
        .unwrap();

        let data: DocumentFields = [("title".to_string(), json!("hello"))]
            .into_iter()
            .collect();

        let result = call_field_hook_ref(
            &lua,
            &HookRef::new("hooks.inspect_ctx"),
            &json!("hello"),
            "title",
            &FieldHookMeta {
                collection: "posts",
                operation: "create",
                id: None,
                locale: None,
            },
            &data,
            &data,
        )
        .unwrap();

        assert_eq!(result, json!("posts:title:create"));
    }

    /// Regression (#14): field hooks receive the content `locale` (distinct from
    /// `ui_locale`), threaded through `FieldHooksCall` → `FieldHookMeta`.
    #[test]
    fn field_hook_context_has_content_locale() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.locale_probe"] = function(value, context)

                return context.locale or "<nil>"
            end
        "#,
        )
        .exec()
        .unwrap();

        let data = DocumentFields::new();

        let result = call_field_hook_ref(
            &lua,
            &HookRef::new("hooks.locale_probe"),
            &json!("x"),
            "title",
            &FieldHookMeta {
                collection: "posts",
                operation: "update",
                id: None,
                locale: Some("de"),
            },
            &data,
            &data,
        )
        .unwrap();

        assert_eq!(
            result,
            json!("de"),
            "field hook must see the content locale as ctx.locale"
        );
    }

    /// A field hook receives the document `id` on update (parity with the
    /// validator / access contexts).
    #[test]
    fn field_hook_context_has_id() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.id_probe"] = function(value, context)

                return context.id or "<nil>"
            end
        "#,
        )
        .exec()
        .unwrap();

        let data = DocumentFields::new();

        let result = call_field_hook_ref(
            &lua,
            &HookRef::new("hooks.id_probe"),
            &json!("x"),
            "title",
            &FieldHookMeta {
                collection: "posts",
                operation: "update",
                id: Some("doc-42"),
                locale: None,
            },
            &data,
            &data,
        )
        .unwrap();

        assert_eq!(
            result,
            json!("doc-42"),
            "field hook must see the document id as ctx.id"
        );
    }

    /// Regression (#14 parity): a field hook on a sub-field inside an array row
    /// sees the row as `ctx.data` but the full document as `ctx.document`, so it
    /// can cross-reference fields outside its row.
    #[test]
    fn field_hook_in_row_sees_full_document() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.tag"] = function(value, ctx)

                -- ctx.data is the row (`label`); ctx.document is the whole doc
                -- (top-level `prefix`). Combine both to prove each is in scope.
                return (ctx.document.prefix or "?") .. ":" .. (ctx.data.label or "?")
            end
        "#,
        )
        .exec()
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("prefix", FieldType::Text).build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("label", FieldType::Text).build(),
                    FieldDefinition::builder("tagged", FieldType::Text)
                        .hooks(FieldHooks {
                            before_change: vec![HookRef::new("hooks.tag")],
                            ..Default::default()
                        })
                        .build(),
                ])
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("prefix".to_string(), json!("DOC"));
        data.insert(
            "items".to_string(),
            json!([{ "label": "row0", "tagged": "x" }]),
        );

        run_field_hooks_inner(
            &lua,
            &mut data,
            &FieldHooksCall {
                fields: &fields,
                event: FieldHookEvent::BeforeChange,
                collection: "posts",
                operation: "create",
                id: None,
                locale: None,
            },
        )
        .unwrap();

        let rows = data.get("items").unwrap().as_array().unwrap();
        assert_eq!(
            rows[0].get("tagged").unwrap(),
            &json!("DOC:row0"),
            "row hook must see the full document (prefix) and the row (label)"
        );
    }

    /// Regression: `has_any_field_hook` must find hooks inside Group/Row/Tabs.
    #[test]
    fn has_any_field_hook_finds_nested_hooks() {
        let mut inner = FieldDefinition::builder("inner", FieldType::Text).build();
        inner.hooks.before_change = vec![HookRef::new("hooks.my_hook")];

        // Hook on a sub-field inside a Group
        let group = FieldDefinition::builder("group", FieldType::Group)
            .fields(vec![inner.clone()])
            .build();
        assert!(
            has_any_field_hook(&[group], &FieldHookEvent::BeforeChange),
            "should find hook inside Group"
        );

        // Hook on a sub-field inside a Row
        let row = FieldDefinition::builder("row", FieldType::Row)
            .fields(vec![inner.clone()])
            .build();
        assert!(
            has_any_field_hook(&[row], &FieldHookEvent::BeforeChange),
            "should find hook inside Row"
        );

        // Hook on a sub-field inside Tabs
        let tab_field = FieldDefinition::builder("tabs", FieldType::Tabs)
            .tabs(vec![FieldTab {
                label: "Tab1".to_string(),
                description: None,
                fields: vec![inner],
            }])
            .build();
        assert!(
            has_any_field_hook(&[tab_field], &FieldHookEvent::BeforeChange),
            "should find hook inside Tabs"
        );

        // No hook → false
        let plain = FieldDefinition::builder("plain", FieldType::Text).build();
        assert!(
            !has_any_field_hook(&[plain], &FieldHookEvent::BeforeChange),
            "should not find hook on plain field without hooks"
        );
    }
}
