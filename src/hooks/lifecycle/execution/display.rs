//! Display condition evaluation for field visibility.
//!
//! Display conditions are Lua hook functions registered via `field.admin.condition`
//! that decide whether a field is shown in the admin form, given the current form
//! data. The hook returns either a bool, a structured `ConditionExpr` table, or
//! `nil` (no condition → show normally). All error paths fail closed (hide).

use mlua::{Lua, LuaSerdeExt as _, Value};
use serde_json::Value as JsonValue;
use tracing::warn;

use crate::{
    core::ConditionExpr,
    hooks::{
        lifecycle::{ConditionContext, DisplayConditionResult},
        lua_api,
    },
};

use super::resolve_hook_function;

/// Inner implementation of display condition evaluation — operates on a locked `&Lua`.
/// The condition function is called as `fn(form_data, ctx)`; existing
/// `function(form_data)` conditions ignore the extra `ctx` argument.
pub(crate) fn call_display_condition_with_lua(
    lua: &Lua,
    func_ref: &str,
    form_data: &JsonValue,
    ctx: &ConditionContext<'_>,
) -> Option<DisplayConditionResult> {
    let func = resolve_hook_function(lua, func_ref).ok()?;
    let data_lua = lua_api::json_to_lua(lua, form_data).ok()?;
    let ctx_lua = lua.to_value(ctx).ok()?;

    match func.call::<Value>((data_lua, ctx_lua)) {
        Ok(Value::Boolean(b)) => Some(DisplayConditionResult::Bool(b)),
        Ok(val @ Value::Table(_)) => {
            let json = lua_api::lua_to_json(&val).ok()?;

            match serde_json::from_value::<ConditionExpr>(json) {
                Ok(condition) => {
                    let visible = condition.evaluate(form_data);

                    Some(DisplayConditionResult::Table { condition, visible })
                }
                Err(e) => {
                    warn!(
                        "Display condition '{}' returned a malformed condition table: {} — hiding field (fail closed)",
                        func_ref, e
                    );

                    Some(DisplayConditionResult::Bool(false))
                }
            }
        }
        Ok(Value::Nil) => None, // nil → no condition, show field normally
        Err(e) => {
            warn!(
                "Display condition '{}' failed: {} — hiding field (fail closed)",
                func_ref, e
            );

            Some(DisplayConditionResult::Bool(false))
        }
        Ok(other) => {
            warn!(
                "Display condition '{}' returned unexpected type {:?} — hiding field (fail closed)",
                func_ref,
                other.type_name()
            );

            Some(DisplayConditionResult::Bool(false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cond_ctx() -> ConditionContext<'static> {
        ConditionContext {
            collection: "posts",
            operation: "update",
            user: None,
            ui_locale: None,
            locale: None,
            options: None,
        }
    }

    #[test]
    fn display_condition_returns_bool_true() {
        let lua = mlua::Lua::new();
        lua.load(r#"package.loaded["hooks.show"] = function() return true end"#)
            .exec()
            .unwrap();

        let result = call_display_condition_with_lua(&lua, "hooks.show", &json!({}), &cond_ctx());
        assert!(matches!(result, Some(DisplayConditionResult::Bool(true))));
    }

    #[test]
    fn display_condition_returns_bool_false() {
        let lua = mlua::Lua::new();
        lua.load(r#"package.loaded["hooks.hide"] = function() return false end"#)
            .exec()
            .unwrap();

        let result = call_display_condition_with_lua(&lua, "hooks.hide", &json!({}), &cond_ctx());
        assert!(matches!(result, Some(DisplayConditionResult::Bool(false))));
    }

    #[test]
    fn display_condition_returns_nil_shows_field() {
        let lua = mlua::Lua::new();
        lua.load(r#"package.loaded["hooks.nil_ret"] = function() return nil end"#)
            .exec()
            .unwrap();

        let result =
            call_display_condition_with_lua(&lua, "hooks.nil_ret", &json!({}), &cond_ctx());
        assert!(result.is_none(), "nil should show field (None)");
    }

    /// Regression: display condition errors must fail closed (hide field),
    /// not fail open (show field). Showing a field on error could expose
    /// access-controlled content.
    #[test]
    fn display_condition_error_hides_field() {
        let lua = mlua::Lua::new();
        lua.load(r#"package.loaded["hooks.boom"] = function() error("broken") end"#)
            .exec()
            .unwrap();

        let result = call_display_condition_with_lua(&lua, "hooks.boom", &json!({}), &cond_ctx());
        assert!(
            matches!(result, Some(DisplayConditionResult::Bool(false))),
            "error must hide field (fail closed), got {result:?}"
        );
    }

    /// Regression: display condition returning unexpected type must fail closed.
    #[test]
    fn display_condition_unexpected_type_hides_field() {
        let lua = mlua::Lua::new();
        lua.load(r#"package.loaded["hooks.num"] = function() return 42 end"#)
            .exec()
            .unwrap();

        let result = call_display_condition_with_lua(&lua, "hooks.num", &json!({}), &cond_ctx());
        assert!(
            matches!(result, Some(DisplayConditionResult::Bool(false))),
            "unexpected type must hide field (fail closed), got {result:?}"
        );
    }

    #[test]
    fn display_condition_unresolvable_ref_shows_field() {
        let lua = mlua::Lua::new();
        let result =
            call_display_condition_with_lua(&lua, "hooks.nonexistent", &json!({}), &cond_ctx());
        assert!(result.is_none(), "unresolvable reference should show field");
    }

    /// Per-config `options` reach the condition as `ctx.options`. The SAME ref
    /// with DIFFERENT options must yield DIFFERENT results — the property that
    /// lets one condition function be reused across fields, and that the batch
    /// evaluator must key per-field (not per-ref) to preserve.
    #[test]
    fn display_condition_receives_options() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"package.loaded["hooks.want"] = function(_, ctx)
                return ctx.options ~= nil and ctx.options.want == "link"
            end"#,
        )
        .exec()
        .unwrap();

        let want_link = json!({ "want": "link" });
        let ctx_link = ConditionContext {
            options: Some(&want_link),
            ..cond_ctx()
        };
        let r1 = call_display_condition_with_lua(&lua, "hooks.want", &json!({}), &ctx_link);
        assert!(matches!(r1, Some(DisplayConditionResult::Bool(true))));

        let want_video = json!({ "want": "video" });
        let ctx_video = ConditionContext {
            options: Some(&want_video),
            ..cond_ctx()
        };
        let r2 = call_display_condition_with_lua(&lua, "hooks.want", &json!({}), &ctx_video);
        assert!(
            matches!(r2, Some(DisplayConditionResult::Bool(false))),
            "same ref, different options must give a different result, got {r2:?}"
        );
    }

    /// A condition function receives a second `ctx` argument carrying
    /// `operation` / `collection` (etc.), in addition to the form data.
    #[test]
    fn display_condition_receives_context() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["hooks.ctx"] = function(data, ctx)

                return ctx.operation == "update" and ctx.collection == "posts"
            end
        "#,
        )
        .exec()
        .unwrap();

        let result = call_display_condition_with_lua(&lua, "hooks.ctx", &json!({}), &cond_ctx());
        assert!(
            matches!(result, Some(DisplayConditionResult::Bool(true))),
            "condition must receive ctx.operation / ctx.collection, got {result:?}"
        );
    }
}
