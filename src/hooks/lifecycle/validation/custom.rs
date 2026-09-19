use std::collections::HashMap;

use anyhow::{Result, bail};
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
    /// Per-config options from the `validate` / `required_when` hook ref.
    pub options: Option<&'a JsonValue>,
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
        options: src.options,
    };
    let ctx_table = lua.to_value(&ctx)?;

    let result: Value = func.call((lua_value, ctx_table))?;

    validator_verdict(&result, src.field_name)
}

/// Interpret a custom validator's return value: `nil`/`true` is valid,
/// `false` is invalid with the default message, a string is invalid with
/// that message. Any other type is a hook error naming the field — a
/// validator returning `{ error = "bad" }` or `0` used to count as valid,
/// silently passing what it meant to reject.
fn validator_verdict(result: &Value, field_name: &str) -> Result<Option<String>> {
    match result {
        Value::Nil | Value::Boolean(true) => Ok(None),
        Value::Boolean(false) => Ok(Some("validation failed".to_string())),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        other => bail!(
            "validator for field '{field_name}' must return nil, true, false or a message; \
             got {}",
            other.type_name()
        ),
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
        options: src.options,
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
                options: None,
            },
        )
        .unwrap();
        assert!(result.is_none());
    }

    fn src(data: &HashMap<String, JsonValue>) -> ValidateCtxSource<'_> {
        ValidateCtxSource {
            data,
            document: data,
            collection: "test",
            field_name: "name",
            locale: None,
            operation: "create",
            id: None,
            options: None,
        }
    }

    /// A validator returning a number or a table used to count as VALID — a
    /// `return { error = "bad" }` or `return 0` silently passed. Any type
    /// outside the contract is a hook error naming the field.
    #[test]
    fn validator_returning_an_unexpected_type_is_a_hook_error() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                number = function(value, ctx) return 0 end,
                table = function(value, ctx) return { error = "bad" } end,
            }
        "#,
        )
        .exec()
        .unwrap();
        let data = HashMap::new();

        for validator in ["validators.number", "validators.table"] {
            let err = run_validate_function_inner(&lua, validator, &json!("x"), &src(&data))
                .expect_err("an out-of-contract return must be a hook error")
                .to_string();
            assert!(err.contains("field 'name'"), "names the field: {err}");
            assert!(
                err.contains("nil, true, false or a message"),
                "states the contract: {err}"
            );
        }
    }

    /// The full contract: nil/true valid, false = default message, string =
    /// that message.
    #[test]
    fn validator_contract_true_false_and_message() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                yes = function(value, ctx) return true end,
                no = function(value, ctx) return false end,
                msg = function(value, ctx) return "too short" end,
            }
        "#,
        )
        .exec()
        .unwrap();
        let data = HashMap::new();

        let run = |validator: &str| {
            run_validate_function_inner(&lua, validator, &json!("x"), &src(&data)).unwrap()
        };

        assert_eq!(run("validators.yes"), None);
        assert_eq!(run("validators.no").as_deref(), Some("validation failed"));
        assert_eq!(run("validators.msg").as_deref(), Some("too short"));
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
                options: None,
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
