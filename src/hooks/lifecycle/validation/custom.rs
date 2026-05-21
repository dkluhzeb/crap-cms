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
pub(super) fn run_validate_function_inner(
    lua: &Lua,
    func_ref: &str,
    value: &JsonValue,
    data: &HashMap<String, JsonValue>,
    collection: &str,
    field_name: &str,
) -> Result<Option<String>> {
    let func = resolve_hook_function(lua, func_ref)?;
    let lua_value = lua_api::json_to_lua(lua, value)?;

    let user_ctx_ref = lua.app_data_ref::<UserContext>();
    let locale_ctx_ref = lua.app_data_ref::<UiLocaleContext>();
    let ctx = crate::hooks::lifecycle::ValidateContext {
        collection,
        field_name,
        data,
        user: user_ctx_ref.as_ref().and_then(|c| c.0.as_ref()),
        ui_locale: locale_ctx_ref.as_ref().and_then(|c| c.0.as_deref()),
    };
    let ctx_table = lua.to_value(&ctx)?;

    let result: Value = func.call((lua_value, ctx_table))?;
    match result {
        Value::Boolean(false) => Ok(Some("validation failed".to_string())),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        _ => Ok(None),
    }
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
            &data,
            "test",
            "name",
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
            &data,
            "test",
            "name",
        )
        .unwrap();
        assert!(
            result.is_none(),
            "Number return from validator should be treated as valid (None)"
        );
    }
}
