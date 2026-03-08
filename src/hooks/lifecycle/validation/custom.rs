use std::collections::HashMap;

use anyhow::Result;
use mlua::{Lua, Value};

use super::super::execution::resolve_hook_function;

/// Inner implementation of `run_validate_function` — operates on a locked `&Lua`.
/// Used by both `HookRunner::validate_fields` and Lua CRUD closures.
pub(super) fn run_validate_function_inner(
    lua: &Lua,
    func_ref: &str,
    value: &serde_json::Value,
    data: &HashMap<String, serde_json::Value>,
    collection: &str,
    field_name: &str,
) -> Result<Option<String>> {
    let func = resolve_hook_function(lua, func_ref)?;
    let lua_value = crate::hooks::api::json_to_lua(lua, value)?;
    let ctx_table = lua.create_table()?;
    ctx_table.set("collection", collection)?;
    ctx_table.set("field_name", field_name)?;
    let data_table = lua.create_table()?;
    for (k, v) in data {
        data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("data", data_table)?;

    let result: Value = func.call((lua_value, ctx_table))?;
    match result {
        Value::Nil => Ok(None),
        Value::Boolean(true) => Ok(None),
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
        lua.load(r#"
            package.loaded["validators"] = {
                validate_nil = function(value, ctx)
                    return nil
                end
            }
        "#).exec().unwrap();
        let data = HashMap::new();
        let result = run_validate_function_inner(
            &lua, "validators.validate_nil", &json!("test"), &data, "test", "name"
        ).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_run_validate_function_other_return_means_valid() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = {
                validate_number = function(value, ctx)
                    return 42  -- a number return is treated as valid
                end
            }
        "#).exec().unwrap();
        let data = HashMap::new();
        let result = run_validate_function_inner(
            &lua, "validators.validate_number", &json!("test"), &data, "test", "name"
        ).unwrap();
        assert!(result.is_none(), "Number return from validator should be treated as valid (None)");
    }
}
