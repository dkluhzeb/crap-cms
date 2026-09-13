//! `crap.validation_error` — the structured way for a hook to reject a write.
//!
//! A Lua hook can only fail by raising a value, and a plain `error("…")`
//! reaches the caller as an opaque hook failure. This raises one that every
//! surface decodes back into per-field validation errors, so the message
//! lands on the offending input rather than in a generic banner.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult, Table, Value};

use crate::{
    core::validate::{FieldError, ValidationError},
    typegen::lua::{LuaFnSpec, LuaParam, LuaReturn, lua_fn, lua_table},
};

/// Raise a structured validation error from a hook.
///
/// Takes a table of field name to message — `{ title = "title is required" }`
/// — and never returns: it raises, aborting the write. Every surface reports
/// it the way it reports a built-in validation failure (gRPC
/// `INVALID_ARGUMENT`, a field error on the admin form), rather than as an
/// opaque hook error.
#[lua_fn(
    path = "crap.validation_error",
    returns = "nil",
    returns_doc = "Never returns — always raises."
)]
fn validation_error_fn(
    _: &Lua,
    #[lua(doc = "Map of field name to error message.")] errors: Table,
) -> LuaResult<()> {
    let mut fields = Vec::new();

    // Keys as `Value`, not `String`: mlua would coerce a number, so the
    // positional `{ "title is required" }` a hook author reaches for by
    // mistake would silently become an error on a field named "1".
    for pair in errors.pairs::<Value, Value>() {
        let (key, value) = pair?;

        let Value::String(field) = key else {
            return Err(RuntimeError(
                "crap.validation_error: keys must be field names, e.g. \
                 { title = \"title is required\" }"
                    .to_string(),
            ));
        };
        let field = field.to_string_lossy();

        // Only strings. Anything else was formatted with `{:?}`, which for a
        // table renders mlua's internal `Ref(0x…)` — a live heap address, in
        // a message that travels to the client and onto an admin form.
        let Value::String(message) = value else {
            return Err(RuntimeError(format!(
                "crap.validation_error: the message for '{field}' must be a string"
            )));
        };

        fields.push(FieldError::new(field, message.to_string_lossy()));
    }

    if fields.is_empty() {
        return Err(RuntimeError(
            "crap.validation_error: needs at least one field message".to_string(),
        ));
    }

    // Sort so the message is stable across runs — Lua table iteration order
    // is unspecified, and an error string that reshuffles is untestable.
    fields.sort_by(|a, b| a.field.cmp(&b.field));

    Err(RuntimeError(ValidationError::new(fields).to_hook_message()))
}

lua_table! {
    name: crap_validation,
    path: "crap",
    state: (),
    fns: [validation_error_fn],
}

/// Register `crap.validation_error`. Parent `crap` table must already be in
/// globals.
///
/// # Errors
///
/// Returns an error if the Lua registration fails.
pub(super) fn register_validation(lua: &Lua) -> Result<()> {
    register_crap_validation(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mlua::Lua;

    use super::register_validation;
    use crate::core::validate::ValidationError;

    fn lua_with_crap() -> Lua {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        lua.globals().set("crap", crap).unwrap();
        register_validation(&lua).unwrap();
        lua
    }

    /// The raised message decodes back into the fields the hook named.
    #[test]
    fn a_raised_error_round_trips_into_field_errors() {
        let lua = lua_with_crap();
        let err = lua
            .load(r#"crap.validation_error({ title = "title is required", slug = "taken" })"#)
            .exec()
            .expect_err("validation_error always raises");

        let decoded = ValidationError::from_hook_message(&err.to_string())
            .expect("the message must decode into field errors");

        let fields: Vec<&str> = decoded.errors.iter().map(|e| e.field.as_str()).collect();
        assert_eq!(fields, ["slug", "title"]);
        assert_eq!(decoded.to_field_map()["title"], "title is required");
    }

    /// A hook's message is shown verbatim: it carries no translation key,
    /// because an unknown key renders as the key itself and would swallow
    /// what the hook author actually wrote.
    #[test]
    fn a_hook_message_carries_no_translation_key() {
        let lua = lua_with_crap();
        let err = lua
            .load(r#"crap.validation_error({ title = "pick a shorter title" })"#)
            .exec()
            .expect_err("validation_error always raises");

        let decoded = ValidationError::from_hook_message(&err.to_string()).unwrap();
        assert_eq!(decoded.errors[0].message, "pick a shorter title");
        assert!(decoded.errors[0].key.is_none());
    }

    /// A non-string message used to be rendered with `{:?}`, which for a
    /// table is mlua's `Ref(0x…)` — a live heap address in a string that
    /// reaches the client. And a positional entry must not become a field
    /// named "1".
    #[test]
    fn only_string_keys_and_string_messages_are_accepted() {
        let lua = lua_with_crap();

        let err = lua
            .load(r"crap.validation_error({ title = {} })")
            .exec()
            .expect_err("a table message must raise");
        assert!(err.to_string().contains("must be a string"));
        assert!(!err.to_string().contains("Ref(0x"), "no heap address");

        let err = lua
            .load(r#"crap.validation_error({ "title is required" })"#)
            .exec()
            .expect_err("a positional entry must raise");
        assert!(err.to_string().contains("must be field names"));
    }

    /// Document content that reaches an error message must not be able to
    /// impersonate the channel: a hook doing `error("rejected: " .. slug)`
    /// with an attacker-chosen slug would otherwise turn into a validation
    /// failure on a field of the attacker's choosing.
    #[test]
    fn a_forged_marker_in_user_content_does_not_decode() {
        let lua = lua_with_crap();
        let forged = r#"crap:validation-error:{"email":"already taken"}"#;
        let err = lua
            .load(format!("error('rejected: {forged}')"))
            .exec()
            .expect_err("the hook raises");

        assert!(
            ValidationError::from_hook_message(&err.to_string()).is_none(),
            "guessing the marker must not be enough"
        );
    }

    /// An empty table is a mistake in the hook, not a silent no-op that would
    /// let the write through.
    #[test]
    fn an_empty_table_is_rejected() {
        let lua = lua_with_crap();
        let err = lua
            .load("crap.validation_error({})")
            .exec()
            .expect_err("an empty table must raise");

        assert!(err.to_string().contains("at least one field"));
        assert!(ValidationError::from_hook_message(&err.to_string()).is_none());
    }
}
