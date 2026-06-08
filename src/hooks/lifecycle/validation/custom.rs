use std::collections::HashMap;

use anyhow::Result;
use mlua::{Lua, LuaSerdeExt as _, Value};
use serde_json::Value as JsonValue;

use crate::hooks::{
    lifecycle::{UiLocaleContext, UserContext, execution::resolve_hook_function},
    lua_api,
};

/// Inner implementation of `run_validate_function` — operates on a locked `&Lua`.
/// Used by both `HookRunner::validate_fields` and Lua CRUD closures.
///
/// `data` is the surrounding context map — for top-level field validators this
/// is the document field map (deref'd from `DocumentFields`); for richtext node
/// attribute validators it is the node's attribute map; for array sub-fields
/// it is the array row map. All three pass through opaquely to the user's
/// Lua function.
/// Source fields for building a [`ValidateContext`]. Bundled so the inner
/// validate / predicate functions stay within the argument-count budget as
/// `operation` and `id` were added.
pub(super) struct ValidateCtxSource<'a> {
    pub data: &'a HashMap<String, JsonValue>,
    /// The full document (equals `data` for top-level fields; the parent
    /// document for sub-field validators inside array/blocks rows).
    pub document: &'a HashMap<String, JsonValue>,
    pub collection: &'a str,
    pub field_name: &'a str,
    pub locale: Option<&'a str>,
    pub operation: &'a str,
    pub id: Option<&'a str>,
}

pub(super) fn run_validate_function_inner(
    lua: &Lua,
    func_ref: &str,
    value: &JsonValue,
    src: &ValidateCtxSource<'_>,
) -> Result<Option<String>> {
    let func = resolve_hook_function(lua, func_ref)?;
    let lua_value = lua_api::json_to_lua(lua, value)?;

    let user_ctx_ref = lua.app_data_ref::<UserContext>();
    let locale_ctx_ref = lua.app_data_ref::<UiLocaleContext>();
    let ctx = crate::hooks::lifecycle::ValidateContext {
        collection: src.collection,
        field_name: src.field_name,
        operation: src.operation,
        id: src.id,
        data: src.data,
        document: src.document,
        user: user_ctx_ref.as_ref().and_then(|c| c.0.as_ref()),
        ui_locale: locale_ctx_ref.as_ref().and_then(|c| c.0.as_deref()),
        locale: src.locale,
    };
    let ctx_table = lua.to_value(&ctx)?;

    let result: Value = func.call((lua_value, ctx_table))?;
    match result {
        Value::Boolean(false) => Ok(Some("validation failed".to_string())),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        _ => Ok(None),
    }
}

/// Evaluate a `required_when` predicate ref against the document. The predicate
/// receives the validate context (`ctx.data` = full document) and returns a
/// truthy value when the field should be required. Lua truthiness applies:
/// required unless the predicate returns `nil` or `false`.
pub(super) fn run_required_condition_inner(
    lua: &Lua,
    func_ref: &str,
    src: &ValidateCtxSource<'_>,
) -> Result<bool> {
    let func = resolve_hook_function(lua, func_ref)?;

    let user_ctx_ref = lua.app_data_ref::<UserContext>();
    let locale_ctx_ref = lua.app_data_ref::<UiLocaleContext>();
    let ctx = crate::hooks::lifecycle::ValidateContext {
        collection: src.collection,
        field_name: src.field_name,
        operation: src.operation,
        id: src.id,
        data: src.data,
        document: src.document,
        user: user_ctx_ref.as_ref().and_then(|c| c.0.as_ref()),
        ui_locale: locale_ctx_ref.as_ref().and_then(|c| c.0.as_deref()),
        locale: src.locale,
    };
    let ctx_table = lua.to_value(&ctx)?;

    let result: Value = func.call(ctx_table)?;

    Ok(!matches!(result, Value::Nil | Value::Boolean(false)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_run_validate_function_nil_means_valid() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                validate_nil = function(value, ctx)

                    return nil
                end
            }
        "#,
        )
        .exec()
        .unwrap();
        let data = HashMap::new();
        let result = run_validate_function_inner(
            &lua,
            "validators.validate_nil",
            &json!("test"),
            &ValidateCtxSource {
                data: &data,
                document: &data,
                collection: "test",
                field_name: "name",
                locale: None,
                operation: "create",
                id: None,
            },
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_run_validate_function_other_return_means_valid() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                validate_number = function(value, ctx)

                    return 42  -- a number return is treated as valid
                end
            }
        "#,
        )
        .exec()
        .unwrap();
        let data = HashMap::new();
        let result = run_validate_function_inner(
            &lua,
            "validators.validate_number",
            &json!("test"),
            &ValidateCtxSource {
                data: &data,
                document: &data,
                collection: "test",
                field_name: "name",
                locale: None,
                operation: "create",
                id: None,
            },
        )
        .unwrap();
        assert!(
            result.is_none(),
            "Number return from validator should be treated as valid (None)"
        );
    }

    /// A custom validator receives the content `ctx.locale` so it can enforce
    /// per-locale rules. Here the validator echoes `ctx.locale` back as its
    /// (string) result, proving the locale reached the Lua context.
    #[test]
    fn validate_function_receives_content_locale() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                echo_locale = function(value, ctx)
                    return ctx.locale
                end
            }
        "#,
        )
        .exec()
        .unwrap();
        let data = HashMap::new();

        let result = run_validate_function_inner(
            &lua,
            "validators.echo_locale",
            &json!("test"),
            &ValidateCtxSource {
                data: &data,
                document: &data,
                collection: "posts",
                field_name: "title",
                locale: Some("de"),
                operation: "create",
                id: None,
            },
        )
        .unwrap();
        assert_eq!(
            result.as_deref(),
            Some("de"),
            "validator should see ctx.locale"
        );
    }
}
