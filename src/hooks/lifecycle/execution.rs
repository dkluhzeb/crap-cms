//! Standalone hook execution functions (inner implementations and helpers).

use anyhow::{Context as _, Result, anyhow, bail};
use mlua::{Lua, Value};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};

use crate::{
    core::{
        Document, FieldDefinition, collection::Hooks, document::DocumentBuilder, field::FieldHooks,
    },
    hooks::{
        api,
        lifecycle::{
            DisplayConditionResult, FieldHookEvent, HookEvent, context::HookContext,
            evaluate_condition_table,
        },
    },
};

/// Context for after-read hook execution, bundling the shared parameters.
pub struct AfterReadCtx<'a> {
    pub hooks: &'a Hooks,
    pub fields: &'a [FieldDefinition],
    pub collection: &'a str,
    pub operation: &'a str,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
}

/// Inner implementation of `apply_after_read` — operates on a locked `&Lua`.
/// Runs field-level after_read hooks, then collection-level, then global registered.
/// On error: logs warning, returns original doc unmodified.
pub(crate) fn apply_after_read_inner(lua: &Lua, ctx: &AfterReadCtx, doc: Document) -> Document {
    let has_field_hooks = ctx.fields.iter().any(|f| !f.hooks.after_read.is_empty());

    let has_collection_hooks = !ctx.hooks.after_read.is_empty();
    let has_registered = has_registered_hooks(lua, "after_read");

    if !has_field_hooks && !has_collection_hooks && !has_registered {
        return doc;
    }

    let mut data: HashMap<String, JsonValue> = doc.fields.clone();
    data.insert("id".to_string(), JsonValue::String(doc.id.to_string()));

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
            ctx.fields,
            &FieldHookEvent::AfterRead,
            &mut data,
            ctx.collection,
            ctx.operation,
        )
    {
        tracing::warn!("field after_read hook error for {}: {}", ctx.collection, e);

        return doc;
    }

    let hook_ctx = HookContext::builder(ctx.collection, ctx.operation)
        .data(data)
        .user(ctx.user)
        .ui_locale(ctx.ui_locale)
        .build();

    // Run collection-level + global registered hooks
    let hook_refs = get_hook_refs(ctx.hooks, &HookEvent::AfterRead);
    let result = (|| -> Result<HookContext> {
        let mut context = hook_ctx;
        for hook_ref in hook_refs {
            context = call_hook_ref(lua, hook_ref, context)?;
        }
        context = call_registered_hooks(lua, &HookEvent::AfterRead, context)?;
        Ok(context)
    })();

    match result {
        Ok(result_ctx) => {
            let mut fields = result_ctx.data;
            fields.remove("id");
            let created_at = fields
                .remove("created_at")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .or(doc.created_at.clone());
            let updated_at = fields
                .remove("updated_at")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .or(doc.updated_at.clone());

            DocumentBuilder::new(doc.id)
                .fields(fields)
                .created_at(created_at)
                .updated_at(updated_at)
                .build()
        }
        Err(e) => {
            tracing::warn!("after_read hook error for {}: {}", ctx.collection, e);
            doc
        }
    }
}

/// Inner implementation of `run_hooks` / `run_hooks_with_conn` — operates on a locked `&Lua`.
/// Runs collection-level hook refs, then global registered hooks.
/// TxContext must already be set in app_data if CRUD access is needed.
pub(crate) fn run_hooks_inner(
    lua: &Lua,
    hooks: &Hooks,
    event: HookEvent,
    mut context: HookContext,
) -> Result<HookContext> {
    let hook_refs = get_hook_refs(hooks, &event);

    for hook_ref in hook_refs {
        tracing::debug!(
            "Running hook (inner): {} for {}",
            hook_ref,
            context.collection
        );
        context = call_hook_ref(lua, hook_ref, context)?;
    }

    // Run global registered hooks
    context = call_registered_hooks(lua, &event, context)?;

    Ok(context)
}

/// Get the list of hook references for a given event.
pub(crate) fn get_hook_refs<'a>(hooks: &'a Hooks, event: &HookEvent) -> &'a [String] {
    match event {
        HookEvent::BeforeValidate => &hooks.before_validate,
        HookEvent::BeforeChange => &hooks.before_change,
        HookEvent::AfterChange => &hooks.after_change,
        HookEvent::BeforeRead => &hooks.before_read,
        HookEvent::AfterRead => &hooks.after_read,
        HookEvent::BeforeDelete => &hooks.before_delete,
        HookEvent::AfterDelete => &hooks.after_delete,
        HookEvent::BeforeBroadcast => &hooks.before_broadcast,
        HookEvent::BeforeRender => &[], // global-only, no collection-level refs
    }
}

/// Check if any globally registered hooks exist for the given event.
pub(crate) fn has_registered_hooks(lua: &Lua, event: &str) -> bool {
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return false,
    };
    match event_hooks.get::<Value>(event) {
        Ok(Value::Table(t)) => t.raw_len() > 0,
        _ => false,
    }
}

/// Inner implementation of display condition evaluation — operates on a locked `&Lua`.
pub(crate) fn call_display_condition_with_lua(
    lua: &Lua,
    func_ref: &str,
    form_data: &JsonValue,
) -> Option<DisplayConditionResult> {
    let func = resolve_hook_function(lua, func_ref).ok()?;
    let data_lua = api::json_to_lua(lua, form_data).ok()?;
    match func.call::<Value>(data_lua) {
        Ok(Value::Boolean(b)) => Some(DisplayConditionResult::Bool(b)),
        Ok(val @ Value::Table(_)) => {
            let json = api::lua_to_json(lua, &val).ok()?;
            let visible = evaluate_condition_table(&json, form_data);
            Some(DisplayConditionResult::Table {
                condition: json,
                visible,
            })
        }
        _ => None, // error or nil → show field (safe default)
    }
}

/// Check if any fields have hooks registered for the given field-level event.
pub(crate) fn has_field_hooks_for_event(
    fields: &[FieldDefinition],
    event: &FieldHookEvent,
) -> bool {
    fields.iter().any(|f| {
        let hooks = &f.hooks;
        match event {
            FieldHookEvent::BeforeValidate => !hooks.before_validate.is_empty(),
            FieldHookEvent::BeforeChange => !hooks.before_change.is_empty(),
            FieldHookEvent::AfterChange => !hooks.after_change.is_empty(),
            FieldHookEvent::AfterRead => !hooks.after_read.is_empty(),
        }
    })
}

/// Scan a Lua VM's `_crap_event_hooks` table and return the set of event names
/// that have at least one registered handler. Called once during HookRunner::new().
pub(crate) fn scan_registered_events(lua: &Lua) -> HashSet<String> {
    let mut events = HashSet::new();
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return events,
    };
    for pair in event_hooks.pairs::<String, Value>() {
        if let Ok((key, Value::Table(t))) = pair
            && t.raw_len() > 0
        {
            events.insert(key);
        }
    }
    events
}

/// Call a before_broadcast hook ref. Returns Some(context) to continue, None to suppress.
pub(crate) fn call_before_broadcast_hook(
    lua: &Lua,
    hook_ref: &str,
    context: HookContext,
) -> Result<Option<HookContext>> {
    let func = resolve_hook_function(lua, hook_ref)?;

    let ctx_table = context.to_lua_table(lua)?;
    let result: Value = func.call(ctx_table)?;

    match result {
        Value::Boolean(false) | Value::Nil => Ok(None),
        Value::Table(tbl) => {
            let data_result: mlua::Result<mlua::Table> = tbl.get("data");

            if let Ok(data_tbl) = data_result {
                let mut new_data = HashMap::new();
                for pair in data_tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    new_data.insert(k, api::lua_to_json(lua, &v)?);
                }
                let mut ctx = context;
                ctx.data = new_data;
                Ok(Some(ctx))
            } else {
                Ok(Some(context))
            }
        }
        _ => Ok(Some(context)),
    }
}

/// Call all globally registered before_broadcast hooks.
/// Returns Some(context) to continue, None if any hook suppresses.
pub(crate) fn call_registered_before_broadcast(
    lua: &Lua,
    mut context: HookContext,
) -> Result<Option<HookContext>> {
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return Ok(Some(context)),
    };

    let list: mlua::Table = match event_hooks.get::<Value>("before_broadcast") {
        Ok(Value::Table(t)) => t,
        _ => return Ok(Some(context)),
    };

    let len = list.raw_len();

    if len == 0 {
        return Ok(Some(context));
    }

    for i in 1..=len {
        let func: mlua::Function = list.raw_get(i).with_context(|| {
            format!(
                "registered before_broadcast hook at index {} is not a function",
                i
            )
        })?;

        let ctx_table = context.to_lua_table(lua)?;

        let result: Value = func.call(ctx_table)?;

        match result {
            Value::Boolean(false) | Value::Nil => return Ok(None),
            Value::Table(tbl) => {
                let data_result: mlua::Result<mlua::Table> = tbl.get("data");

                if let Ok(data_tbl) = data_result {
                    let mut new_data = HashMap::new();
                    for pair in data_tbl.pairs::<String, Value>() {
                        let (k, v) = pair?;
                        new_data.insert(k, api::lua_to_json(lua, &v)?);
                    }
                    context.data = new_data;
                }
            }
            _ => {}
        }
    }

    Ok(Some(context))
}

/// Call all globally registered hooks for a given event.
/// Iterates `_crap_event_hooks[event]` and calls each function with the context.
/// Reuses the same context-to-table / table-to-context conversion as `call_hook_ref`.
pub(crate) fn call_registered_hooks(
    lua: &Lua,
    event: &HookEvent,
    mut context: HookContext,
) -> Result<HookContext> {
    let globals = lua.globals();
    let event_hooks: mlua::Table = match globals.get("_crap_event_hooks") {
        Ok(t) => t,
        Err(_) => return Ok(context),
    };

    let list: mlua::Table = match event_hooks.get::<Value>(event.as_str()) {
        Ok(Value::Table(t)) => t,
        _ => return Ok(context),
    };

    let len = list.raw_len();

    if len == 0 {
        return Ok(context);
    }

    for i in 1..=len {
        let func: mlua::Function = list
            .raw_get(i)
            .with_context(|| format!("registered hook at index {} is not a function", i))?;

        tracing::debug!(
            "Running registered {} hook #{} for {}",
            event.as_str(),
            i,
            context.collection
        );

        let ctx_table = context.to_lua_table(lua)?;

        let result: Value = func.call(ctx_table)?;

        if let Value::Table(tbl) = result {
            let data_result: mlua::Result<mlua::Table> = tbl.get("data");

            if let Ok(data_tbl) = data_result {
                let mut new_data = HashMap::new();
                for pair in data_tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    new_data.insert(k, api::lua_to_json(lua, &v)?);
                }
                context.data = new_data;
            }
            context.read_context_back(lua, &tbl);
        }
    }

    Ok(context)
}

/// Shared implementation for `run_field_hooks` and `run_field_hooks_with_conn`.
/// Caller is responsible for locking the Lua VM and (optionally) setting TxContext.
pub(crate) fn run_field_hooks_inner(
    lua: &Lua,
    fields: &[FieldDefinition],
    event: &FieldHookEvent,
    data: &mut HashMap<String, JsonValue>,
    collection: &str,
    operation: &str,
) -> Result<()> {
    for field in fields {
        let hook_refs = get_field_hook_refs(&field.hooks, event);

        if hook_refs.is_empty() {
            continue;
        }

        let was_present = data.contains_key(&field.name);
        let value = data.get(&field.name).cloned().unwrap_or(JsonValue::Null);

        let mut current = value;
        for hook_ref in hook_refs {
            tracing::debug!(
                "Running field hook: {} for {}.{}",
                hook_ref,
                collection,
                field.name
            );
            current = call_field_hook_ref(
                lua,
                hook_ref,
                current,
                &field.name,
                collection,
                operation,
                data,
            )?;
        }

        // Only write back if the field was already in the data, or the hook
        // produced a non-null value (e.g. auto_slug generating a slug on create).
        // Without this, absent fields on partial updates get coerced to Null,
        // which breaks the "skip required check for absent fields" logic.
        if was_present || !current.is_null() {
            data.insert(field.name.clone(), current);
        }
    }

    Ok(())
}

/// Get the list of field hook references for a given event.
pub(crate) fn get_field_hook_refs<'a>(
    hooks: &'a FieldHooks,
    event: &FieldHookEvent,
) -> &'a Vec<String> {
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
    data: &HashMap<String, JsonValue>,
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

    // Call: new_value = hook(value, context)
    let result: Value = func.call((lua_value, ctx_table))?;

    // Convert result back to JSON
    api::lua_to_json(lua, &result)
        .map_err(|e| anyhow!("Field hook '{}' returned invalid value: {}", hook_ref, e))
}

/// Resolve a hook reference to a Lua function.
///
/// Tries file-per-hook first: `require("hooks.posts.auto_slug")` → function.
/// Falls back to module pattern: `require("hooks.posts")["auto_slug"]`.
pub(crate) fn resolve_hook_function(lua: &Lua, hook_ref: &str) -> Result<mlua::Function> {
    let require: mlua::Function = lua.globals().get("require")?;

    // Try file-per-hook: require("hooks.posts.auto_slug") → function
    if let Ok(Value::Function(f)) = require.call::<Value>(hook_ref) {
        return Ok(f);
    }

    // Fallback: module.function pattern
    let parts: Vec<&str> = hook_ref.split('.').collect();

    if parts.len() < 2 {
        bail!("Hook ref '{}' must be module.function format", hook_ref);
    }
    let module_path = parts[..parts.len() - 1].join(".");
    let func_name = parts[parts.len() - 1];

    let module: mlua::Table = require
        .call(module_path.clone())
        .with_context(|| format!("Failed to require module '{}'", module_path))?;
    let func: mlua::Function = module.get(func_name).with_context(|| {
        format!(
            "Function '{}' not found in module '{}'",
            func_name, module_path
        )
    })?;
    Ok(func)
}

/// Resolve a dotted function reference (e.g., "hooks.posts.auto_slug")
/// and call it with the context.
pub(crate) fn call_hook_ref(
    lua: &Lua,
    hook_ref: &str,
    context: HookContext,
) -> Result<HookContext> {
    let func = resolve_hook_function(lua, hook_ref)?;

    // Convert context to Lua table
    let ctx_table = context.to_lua_table(lua)?;

    // Call the hook
    let result: Value = func.call(ctx_table)?;

    // Parse the result back
    match result {
        Value::Table(tbl) => {
            let mut new_ctx = context;
            let data_result: mlua::Result<mlua::Table> = tbl.get("data");

            if let Ok(data_tbl) = data_result {
                let mut new_data = HashMap::new();
                for pair in data_tbl.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    new_data.insert(k, api::lua_to_json(lua, &v)?);
                }
                new_ctx.data = new_data;
            }
            new_ctx.read_context_back(lua, &tbl);
            Ok(new_ctx)
        }
        _ => Ok(context),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldHooks, FieldType};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn apply_after_read_no_hooks_returns_unchanged() {
        let lua = mlua::Lua::new();
        lua.load("_crap_event_hooks = {}").exec().unwrap();
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
            user: None,
            ui_locale: None,
        };
        let result = apply_after_read_inner(&lua, &ctx, doc.clone());
        assert_eq!(result.id, "doc1");
        assert_eq!(result.get_str("title"), Some("Hello"));
    }

    #[test]
    fn has_registered_hooks_empty() {
        let lua = mlua::Lua::new();
        lua.load("_crap_event_hooks = {}").exec().unwrap();
        assert!(!has_registered_hooks(&lua, "after_read"));
    }

    #[test]
    fn has_registered_hooks_with_hooks() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            _crap_event_hooks = {
                after_read = { function() end }
            }
        "#,
        )
        .exec()
        .unwrap();
        assert!(has_registered_hooks(&lua, "after_read"));
        assert!(!has_registered_hooks(&lua, "before_change"));
    }

    #[test]
    fn has_registered_hooks_no_global() {
        let lua = mlua::Lua::new();
        assert!(!has_registered_hooks(&lua, "after_read"));
    }

    #[test]
    fn get_hook_refs_maps_events() {
        let hooks = Hooks {
            before_validate: vec!["hooks.validate".to_string()],
            before_change: vec!["hooks.change".to_string()],
            after_change: vec!["hooks.after".to_string()],
            before_read: vec![],
            after_read: vec!["hooks.read".to_string()],
            before_delete: vec![],
            after_delete: vec![],
            before_broadcast: vec!["hooks.broadcast".to_string()],
        };

        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::BeforeValidate),
            &["hooks.validate"]
        );
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::BeforeChange),
            &["hooks.change"]
        );
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::AfterChange),
            &["hooks.after"]
        );
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeRead).is_empty());
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::AfterRead),
            &["hooks.read"]
        );
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeDelete).is_empty());
        assert!(get_hook_refs(&hooks, &HookEvent::AfterDelete).is_empty());
        assert_eq!(
            get_hook_refs(&hooks, &HookEvent::BeforeBroadcast),
            &["hooks.broadcast"]
        );
        assert!(get_hook_refs(&hooks, &HookEvent::BeforeRender).is_empty());
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

        let data: HashMap<String, JsonValue> = [("title".to_string(), json!("hello"))]
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

        let data: HashMap<String, JsonValue> = HashMap::new();

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

        let mut data: HashMap<String, JsonValue> = HashMap::new();
        data.insert("content".to_string(), json!("updated"));

        run_field_hooks_inner(
            &lua,
            &fields,
            &FieldHookEvent::BeforeValidate,
            &mut data,
            "posts",
            "update",
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

        let mut data: HashMap<String, JsonValue> = HashMap::new();
        data.insert("title".to_string(), json!("Hello"));

        run_field_hooks_inner(
            &lua,
            &fields,
            &FieldHookEvent::BeforeValidate,
            &mut data,
            "posts",
            "create",
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

        let data: HashMap<String, JsonValue> = [("title".to_string(), json!("hello"))]
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
}
