//! Field-level hook execution.
//!
//! Field hooks run per-field and receive `(value, context)`, returning the new
//! value. The `FieldHookWalker` recurses through Group / Row / Collapsible /
//! Tabs containers transparently, accumulating the `__`-separated prefix for
//! Group fields so hook lookups land at the right `data_key`.

use anyhow::{Result, anyhow};
use mlua::{Lua, Value};
use serde_json::Value as JsonValue;
use tracing::debug;

use crate::{
    core::{DocumentFields, FieldDefinition, FieldType, field::FieldHooks},
    db::query::helpers::prefixed_name,
    hooks::{
        api,
        lifecycle::{
            FieldHookEvent, UiLocaleContext, UserContext, converters::document_to_lua_table,
            runner::FieldHooksCall,
        },
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
/// Caller is responsible for locking the Lua VM and (optionally) setting TxContext.
pub(crate) fn run_field_hooks_inner(
    lua: &Lua,
    data: &mut DocumentFields,
    call: &FieldHooksCall<'_>,
) -> Result<()> {
    FieldHookWalker { lua, call }.walk(data, call.fields, "")
}

/// Iterator state for the recursive field-hook walk. Bundles the per-walk
/// invariants (Lua VM, call descriptor) so the recursive helpers stay at
/// ≤ 3 args + receiver instead of 6+ positional args.
struct FieldHookWalker<'a> {
    lua: &'a Lua,
    call: &'a FieldHooksCall<'a>,
}

impl<'a> FieldHookWalker<'a> {
    /// Recursive field-hook execution with prefix support for nested structures.
    /// Group accumulates prefix (`group__`), Row/Collapsible/Tabs pass through transparently.
    fn walk(
        &self,
        data: &mut DocumentFields,
        fields: &[FieldDefinition],
        prefix: &str,
    ) -> Result<()> {
        for field in fields {
            match field.field_type {
                FieldType::Group => {
                    let new_prefix = prefixed_name(prefix, &field.name);
                    self.walk(data, &field.fields, &new_prefix)?;
                }

                FieldType::Row | FieldType::Collapsible => {
                    self.walk(data, &field.fields, prefix)?;
                }

                FieldType::Tabs => {
                    for tab in &field.tabs {
                        self.walk(data, &tab.fields, prefix)?;
                    }
                }

                _ => {
                    self.run_single(data, field, prefix)?;
                }
            }
        }

        Ok(())
    }

    /// Run hooks for a single (non-container) field, using the prefixed data key.
    fn run_single(
        &self,
        data: &mut DocumentFields,
        field: &FieldDefinition,
        prefix: &str,
    ) -> Result<()> {
        let hook_refs = get_field_hook_refs(&field.hooks, &self.call.event);

        if hook_refs.is_empty() {
            return Ok(());
        }

        let data_key = prefixed_name(prefix, &field.name);

        let was_present = data.contains_key(&data_key);
        let value = data.get(&data_key).cloned().unwrap_or(JsonValue::Null);

        let mut current = value;
        let timing = tracing::enabled!(tracing::Level::DEBUG);
        let start = if timing {
            Some(std::time::Instant::now())
        } else {
            None
        };

        for hook_ref in hook_refs {
            current = call_field_hook_ref(
                self.lua,
                hook_ref,
                current,
                &data_key,
                self.call.collection,
                self.call.operation,
                data,
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
    fields.iter().any(|f| {
        if !get_field_hook_refs(&f.hooks, event).is_empty() {
            return true;
        }

        match f.field_type {
            FieldType::Group | FieldType::Row | FieldType::Collapsible => {
                has_any_field_hook(&f.fields, event)
            }
            FieldType::Tabs => f
                .tabs
                .iter()
                .any(|tab| has_any_field_hook(&tab.fields, event)),
            _ => false,
        }
    })
}

/// Get the list of field hook references for a given event.
pub(crate) fn get_field_hook_refs<'a>(
    hooks: &'a FieldHooks,
    event: &FieldHookEvent,
) -> &'a [String] {
    match event {
        FieldHookEvent::BeforeValidate => &hooks.before_validate,
        FieldHookEvent::BeforeChange => &hooks.before_change,
        FieldHookEvent::AfterChange => &hooks.after_change,
        FieldHookEvent::AfterRead => &hooks.after_read,
    }
}

/// Resolve a hook reference and call it as a field hook.
/// Field hooks receive `(value, context)` and return the new value.
pub(crate) fn call_field_hook_ref(
    lua: &Lua,
    hook_ref: &str,
    value: JsonValue,
    field_name: &str,
    collection: &str,
    operation: &str,
    data: &DocumentFields,
) -> Result<JsonValue> {
    let func = resolve_hook_function(lua, hook_ref)?;

    // Convert the field value to Lua
    let lua_value = api::json_to_lua(lua, &value)?;

    // Build context table
    let ctx_table = lua.create_table()?;
    ctx_table.set("field_name", field_name)?;
    ctx_table.set("collection", collection)?;
    ctx_table.set("operation", operation)?;

    let data_table = lua.create_table()?;

    for (k, v) in data {
        data_table.set(k.as_str(), api::json_to_lua(lua, v)?)?;
    }

    ctx_table.set("data", data_table)?;

    // Inject user and ui_locale from TxContext if available
    if let Some(user_ctx) = lua.app_data_ref::<UserContext>()
        && let Some(ref user) = user_ctx.0
    {
        let user_table = document_to_lua_table(lua, user)?;

        ctx_table.set("user", user_table)?;
    }

    if let Some(locale_ctx) = lua.app_data_ref::<UiLocaleContext>()
        && let Some(ref locale) = locale_ctx.0
    {
        ctx_table.set("ui_locale", locale.as_str())?;
    }

    // Call: new_value = hook(value, context)
    let result: Value = func.call((lua_value, ctx_table))?;

    // Convert result back to JSON
    api::lua_to_json(&result)
        .map_err(|e| anyhow!("Field hook '{}' returned invalid value: {}", hook_ref, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::FieldTab;
    use crate::core::{FieldHooks, FieldType};
    use serde_json::json;

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
            "hooks.upper",
            json!("hello"),
            "title",
            "posts",
            "create",
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
            "hooks.trim",
            JsonValue::Null,
            "title",
            "posts",
            "update",
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
                    before_validate: vec!["hooks.noop".to_string()],
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
                    before_validate: vec!["hooks.default_val".to_string()],
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
            "hooks.inspect_ctx",
            json!("hello"),
            "title",
            "posts",
            "create",
            &data,
        )
        .unwrap();

        assert_eq!(result, json!("posts:title:create"));
    }

    /// Regression: has_any_field_hook must find hooks inside Group/Row/Tabs.
    #[test]
    fn has_any_field_hook_finds_nested_hooks() {
        let mut inner = FieldDefinition::builder("inner", FieldType::Text).build();
        inner.hooks.before_change = vec!["hooks.my_hook".to_string()];

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
