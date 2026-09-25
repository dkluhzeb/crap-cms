//! Strict validation of the Lua `auth` table (runs before parsing).

use anyhow::{Result, bail};
use mlua::{Table, Value};

use crate::hooks::lua_api::parse::{
    auth::parse::parse_activation,
    helpers::{deny_unknown_keys, get_bool, get_optional_hook_ref, get_table},
};

/// Keys accepted on the top-level `auth = { ... }` table.
const AUTH_KEYS: &[&str] = &["enabled", "token_expiry", "methods"];

/// Reject unknown keys in the `auth` sub-table, its `methods` entries (validated
/// per method `type`), and each strategy's `activates_on` discriminator. A typo'd
/// or removed key (the old `disable_local` / `strategies`) fails loudly instead of
/// being silently ignored. `auth = true`/`false` (boolean form) is skipped.
pub(in crate::hooks::lua_api::parse) fn validate_auth_keys(config: &Table) -> Result<()> {
    let Ok(auth_tbl) = get_table(config, "auth") else {
        return Ok(());
    };

    deny_unknown_keys(&auth_tbl, "auth", AUTH_KEYS)?;
    validate_token_expiry(&auth_tbl)?;

    // Strict boolean (nil → default, non-boolean → error). NEVER
    // `get::<bool>` here: mlua coerces a missing key to `false`, which
    // silently turned `auth = { methods = {...} }` into a DISABLED auth
    // collection before this helper was used.
    let enabled = get_bool(&auth_tbl, "enabled", true)?;

    // `methods` must be absent (defaults apply) or a list of tables. A
    // wrong-typed value or a non-table entry is a hard error — both used to
    // be silently discarded, and a discarded-to-empty list then gained the
    // FULL default method set.
    let methods_tbl = match auth_tbl.get::<Value>("methods")? {
        Value::Nil => return Ok(()),
        Value::Table(t) => t,
        other => bail!(
            "auth.methods must be a list of method tables (got {})",
            other.type_name()
        ),
    };

    let mut methods: Vec<Table> = Vec::new();
    for (i, entry) in methods_tbl.sequence_values::<Value>().enumerate() {
        match entry? {
            Value::Table(t) => methods.push(t),
            other => bail!(
                "auth.methods[{}] must be a method table (got {})",
                i + 1,
                other.type_name()
            ),
        }
    }

    // An explicit empty list is a mistake, not a request for the defaults
    // (omit the key for those) — and `enabled = true` with zero methods
    // would otherwise silently gain password_login + bearer + session_cookie.
    if methods.is_empty() && enabled {
        bail!(
            "auth.methods is empty — list at least one method, or omit `methods` to use the defaults (password_login, bearer, session_cookie)"
        );
    }

    for method in &methods {
        validate_method_keys(method)?;
    }

    Ok(())
}

/// `auth.token_expiry`: absent (the global `[auth] token_expiry` applies) or a
/// positive number of seconds. A wrong-typed or non-positive value used to be
/// read as the built-in 7200 without a word — and `0` would mint sessions
/// that are dead on arrival.
fn validate_token_expiry(auth_tbl: &Table) -> Result<()> {
    match auth_tbl.get::<Value>("token_expiry")? {
        Value::Nil => Ok(()),
        Value::Integer(n) if n > 0 => Ok(()),
        other => bail!(
            "auth.token_expiry must be a positive whole number of seconds (got {})",
            describe_value(&other)
        ),
    }
}

/// A short description of a rejected Lua value for an error message.
fn describe_value(value: &Value) -> String {
    match value {
        Value::Integer(n) => n.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.type_name().to_string(),
    }
}

/// A `strategy` method needs a callable `authenticate` and an explicit
/// `activates_on`. Both used to be tolerated: a missing/empty
/// `authenticate` silently DROPPED the method, and a missing
/// `activates_on` silently became `always = true` (a strategy that fires
/// on every request). Both are hard load errors now.
fn validate_strategy_shape(method: &Table) -> Result<()> {
    let name = method.get::<String>("name").unwrap_or_default();

    // Parse the ref for real (string or `{ ref, options }` table) so an
    // empty/missing `ref` inside the table form errors here instead of
    // being silently dropped by `parse_method` later.
    match get_optional_hook_ref(method, "authenticate", "auth strategy") {
        Ok(Some(h)) if !h.reference().is_empty() => {}
        Ok(_) => bail!(
            "auth strategy '{name}': `authenticate` is required (a hook ref string or {{ ref, options }} with a non-empty ref)"
        ),
        Err(e) => return Err(e),
    }

    if parse_activation(method).is_none() {
        bail!(
            "auth strategy '{name}': `activates_on` is required — {{ header = \"x-...\" }} or {{ always = true }}"
        );
    }

    Ok(())
}

/// Validate one `methods` entry against the keys valid for its `type`.
fn validate_method_keys(method: &Table) -> Result<()> {
    let ty: String = method
        .get::<Option<String>>("type")
        .ok()
        .flatten()
        .unwrap_or_default();

    // Fail closed on an unknown mfa string: silently mapping a typo
    // ("emial", "TOTP") to Off would disable a second factor the operator
    // believes is on. `false` arrives as a boolean (not a string) and means
    // Off by design.
    if ty == "password_login"
        && let Ok(Some(mode)) = method.get::<Option<String>>("mfa")
        && !matches!(mode.as_str(), "email" | "custom" | "totp")
    {
        bail!(
            "password_login method: unknown mfa mode '{mode}' \
             (expected \"email\", \"custom\", \"totp\", or false)"
        );
    }

    let allowed: &[&str] = match ty.as_str() {
        "password_login" => &[
            "type",
            "mfa",
            "mfa_when",
            "mfa_deliver",
            "mfa_exempt_callbacks",
            "verify_email",
            "forgot_password",
        ],
        "bearer" | "session_cookie" => &["type", "surfaces"],
        "strategy" => &["type", "name", "authenticate", "activates_on", "surfaces"],
        other => bail!(
            "Unknown auth method type '{other}'. Valid types: password_login, bearer, session_cookie, strategy"
        ),
    };

    deny_unknown_keys(method, &format!("{ty} auth method"), allowed)?;

    if let Ok(activation) = get_table(method, "activates_on") {
        deny_unknown_keys(&activation, "activates_on", &["header", "always"])?;
    }

    validate_surfaces(method)?;

    if ty == "strategy" {
        validate_strategy_shape(method)?;
    }

    if ty == "password_login" {
        get_bool(method, "verify_email", false)?;
        get_bool(method, "forgot_password", true)?;
        validate_exempt_callbacks(method)?;
    }

    Ok(())
}

/// Strict `mfa_exempt_callbacks` validation: absent, or a list of non-empty
/// callback names. A wrong-typed value or entry would otherwise be dropped,
/// silently putting a callback the operator meant to exempt back behind MFA
/// (or, for a typo'd name, exempting nothing without a word).
fn validate_exempt_callbacks(method: &Table) -> Result<()> {
    let list = match method.get::<Value>("mfa_exempt_callbacks")? {
        Value::Nil => return Ok(()),
        Value::Table(t) => t,
        other => bail!(
            "password_login method: `mfa_exempt_callbacks` must be a list of callback names (got {})",
            other.type_name()
        ),
    };

    for entry in list.sequence_values::<Value>() {
        match entry? {
            Value::String(s) if !s.to_str()?.is_empty() => {}
            other => bail!(
                "password_login method: `mfa_exempt_callbacks` entries must be non-empty callback names (got {})",
                other.type_name()
            ),
        }
    }

    Ok(())
}

/// Strict `surfaces` validation: absent is fine (type-specific default), the
/// string `"all"` is the every-surface sentinel, and a list may only contain
/// known surface names — an unknown entry (a typo like `"gprc"`) used to be
/// silently skipped, silently shrinking the method's reach.
fn validate_surfaces(method: &Table) -> Result<()> {
    match method.get::<Value>("surfaces")? {
        Value::Nil => Ok(()),
        Value::String(s) if s.to_str()? == "all" => Ok(()),
        Value::String(other) => bail!(
            "auth method `surfaces` must be \"all\" or a list of surface names (got the string \"{}\")",
            other.to_str()?
        ),
        Value::Table(t) => {
            for entry in t.sequence_values::<Value>() {
                match entry? {
                    Value::String(s) if matches!(&*s.to_str()?, "admin" | "grpc") => {}
                    Value::String(s) => bail!(
                        "auth method `surfaces`: unknown surface \"{}\" (valid: admin, grpc — or the string \"all\")",
                        s.to_str()?
                    ),
                    other => bail!(
                        "auth method `surfaces` entries must be strings (got {})",
                        other.type_name()
                    ),
                }
            }
            Ok(())
        }
        other => bail!(
            "auth method `surfaces` must be \"all\" or a list of surface names (got {})",
            other.type_name()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    use crate::{
        core::collection::AuthMethod, hooks::lua_api::parse::auth::parse::parse_collection_auth,
    };

    /// Regression: an unknown mfa string used to be silently mapped to
    /// `Off` — disabling a second factor the operator believes is on. The
    /// method validator now fails closed.
    #[test]
    fn unknown_mfa_mode_is_rejected() {
        let lua = Lua::new();
        let m = lua.create_table().unwrap();
        m.set("type", "password_login").unwrap();
        m.set("mfa", "TOTP").unwrap();

        let err = validate_method_keys(&m).unwrap_err().to_string();
        assert!(err.contains("unknown mfa mode 'TOTP'"), "{err}");

        // The valid spellings pass.
        for mode in ["email", "custom", "totp"] {
            m.set("mfa", mode).unwrap();
            validate_method_keys(&m).unwrap_or_else(|e| panic!("{mode}: {e}"));
        }
    }

    /// `mfa_exempt_callbacks` parses into the typed name list.
    #[test]
    fn parses_mfa_exempt_callbacks() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "password_login").unwrap();
        m.set("mfa", "totp").unwrap();
        m.set("mfa_exempt_callbacks", vec!["okta", "azure"])
            .unwrap();
        validate_method_keys(&m).unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();

        let auth = parse_collection_auth(&tbl).unwrap();
        let AuthMethod::PasswordLogin {
            mfa_exempt_callbacks,
            ..
        } = &auth.methods[0]
        else {
            panic!("expected PasswordLogin");
        };
        assert_eq!(mfa_exempt_callbacks, &["okta", "azure"]);
    }

    /// A malformed `mfa_exempt_callbacks` fails the load instead of being
    /// silently dropped.
    #[test]
    fn malformed_mfa_exempt_callbacks_are_rejected() {
        let lua = Lua::new();
        let m = lua.create_table().unwrap();
        m.set("type", "password_login").unwrap();

        m.set("mfa_exempt_callbacks", "okta").unwrap();
        let err = validate_method_keys(&m).unwrap_err().to_string();
        assert!(err.contains("must be a list of callback names"), "{err}");

        m.set("mfa_exempt_callbacks", vec![""]).unwrap();
        let err = validate_method_keys(&m).unwrap_err().to_string();
        assert!(err.contains("non-empty callback names"), "{err}");

        m.set("mfa_exempt_callbacks", vec![1]).unwrap();
        assert!(validate_method_keys(&m).is_err());
    }

    fn auth_config(lua: &Lua, build: impl FnOnce(&Table)) -> Result<()> {
        let config = lua.create_table().unwrap();
        let auth = lua.create_table().unwrap();
        build(&auth);
        config.set("auth", auth).unwrap();
        validate_auth_keys(&config)
    }

    /// Regression: an unset `token_expiry` parsed as 7200, so the global
    /// `[auth] token_expiry` never applied; a wrong-typed or non-positive one
    /// was silently read as 7200 too. Unset stays unset, a bad value is a
    /// load error.
    #[test]
    fn token_expiry_is_optional_and_strict() {
        let lua = Lua::new();
        let parse = |value: Option<i64>| {
            let tbl = lua.create_table().unwrap();
            let auth_tbl = lua.create_table().unwrap();
            if let Some(v) = value {
                auth_tbl.set("token_expiry", v).unwrap();
            }
            tbl.set("auth", auth_tbl).unwrap();
            parse_collection_auth(&tbl).unwrap().token_expiry
        };

        assert_eq!(parse(None), None, "unset inherits the global default");
        assert_eq!(parse(Some(600)), Some(600));

        for bad in [
            Value::Integer(0),
            Value::Integer(-5),
            Value::Number(1.5),
            Value::String(lua.create_string("2h").unwrap()),
        ] {
            let err = auth_config(&lua, |a| a.set("token_expiry", bad).unwrap()).unwrap_err();
            assert!(err.to_string().contains("token_expiry"), "{err}");
        }
    }

    /// Regression: `auth = { methods = {...} }` without an explicit
    /// `enabled` parsed as DISABLED (and `password_login` without
    /// `forgot_password` as forgot-password-off) because `get::<bool>`
    /// reads a missing key as `false`. Both must take their documented
    /// `true` default; a non-boolean value is a load error.
    #[test]
    fn auth_table_without_enabled_key_is_enabled() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "password_login").unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        assert!(auth.enabled, "missing `enabled` must default to true");
        assert!(
            auth.password_login().is_some_and(|p| p.forgot_password),
            "missing `forgot_password` must default to true"
        );

        let err = auth_config(&lua, |a| {
            a.set("enabled", "yes").unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("expected a boolean"), "{err}");
    }

    fn strategy_method(lua: &Lua, authenticate: Option<&str>, activates_on: bool) -> Table {
        let m = lua.create_table().unwrap();
        m.set("type", "strategy").unwrap();
        m.set("name", "sso").unwrap();
        if let Some(a) = authenticate {
            m.set("authenticate", a).unwrap();
        }
        if activates_on {
            let act = lua.create_table().unwrap();
            act.set("header", "x-sso").unwrap();
            m.set("activates_on", act).unwrap();
        }
        m
    }

    fn methods_config(lua: &Lua, methods: Vec<Table>) -> Result<()> {
        auth_config(lua, |a| {
            let list = lua.create_table().unwrap();
            for (i, m) in methods.into_iter().enumerate() {
                list.set(i + 1, m).unwrap();
            }
            a.set("methods", list).unwrap();
        })
    }

    /// Regression: `methods = {}` used to silently gain the default method
    /// set (`password_login` + `bearer` + `session_cookie`).
    #[test]
    fn validate_rejects_explicit_empty_methods() {
        let lua = Lua::new();
        let err = methods_config(&lua, vec![]).unwrap_err().to_string();
        assert!(err.contains("auth.methods is empty"), "{err}");

        // `enabled = false` with an empty list is fine (nothing to run).
        auth_config(&lua, |a| {
            a.set("enabled", false).unwrap();
            a.set("methods", lua.create_table().unwrap()).unwrap();
        })
        .unwrap();
    }

    /// Regression: a strategy with a missing/empty `authenticate` used to be
    /// silently dropped by the parser.
    #[test]
    fn validate_rejects_strategy_without_authenticate() {
        let lua = Lua::new();
        for auth in [None, Some("")] {
            let err = methods_config(&lua, vec![strategy_method(&lua, auth, true)])
                .unwrap_err()
                .to_string();
            assert!(err.contains("`authenticate` is required"), "{err}");
        }
    }

    /// Regression: a strategy without `activates_on` used to default to
    /// `always = true` (fires on every request) with only a warning.
    #[test]
    fn validate_rejects_strategy_without_activates_on() {
        let lua = Lua::new();
        let err = methods_config(
            &lua,
            vec![strategy_method(&lua, Some("hooks.auth.sso"), false)],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`activates_on` is required"), "{err}");

        methods_config(
            &lua,
            vec![strategy_method(&lua, Some("hooks.auth.sso"), true)],
        )
        .unwrap();
    }

    /// Regression: a wrong-typed `methods` value (string, number) used to
    /// pass validation, parse to an empty list, and silently gain the FULL
    /// default method set.
    #[test]
    fn validate_rejects_non_table_methods_value() {
        let lua = Lua::new();
        for bad in ["\"password_login\"", "5"] {
            let src = format!("crap = {{}}; return {{ auth = {{ methods = {bad} }} }}");
            let config: Table = lua.load(&src).eval().unwrap();
            let err = validate_auth_keys(&config).unwrap_err().to_string();
            assert!(
                err.contains("must be a list of method tables"),
                "{bad}: {err}"
            );
        }
    }

    /// Regression: a non-table entry inside `methods` (e.g. the string
    /// shorthand `"bearer"`) was silently skipped by the sequence iterator.
    #[test]
    fn validate_rejects_non_table_method_entry() {
        let lua = Lua::new();
        let config: Table = lua
            .load(r#"return { auth = { methods = { { type = "password_login" }, "bearer" } } }"#)
            .eval()
            .unwrap();
        let err = validate_auth_keys(&config).unwrap_err().to_string();
        assert!(err.contains("auth.methods[2]"), "{err}");
    }

    /// Regression: `authenticate = { ref = "" }` (or `{}`) passed the
    /// validator's blanket table-accept and was then silently dropped by the
    /// parser — with a single strategy, the collection fell back to the full
    /// default method set.
    #[test]
    fn validate_rejects_strategy_with_empty_table_ref() {
        let lua = Lua::new();
        let m = strategy_method(&lua, None, true);
        let auth_tbl = lua.create_table().unwrap();
        auth_tbl.set("ref", "").unwrap();
        m.set("authenticate", auth_tbl).unwrap();
        let err = methods_config(&lua, vec![m]).unwrap_err().to_string();
        assert!(err.contains("authenticate"), "{err}");

        let m = strategy_method(&lua, None, true);
        m.set("authenticate", lua.create_table().unwrap()).unwrap();
        let err = methods_config(&lua, vec![m]).unwrap_err().to_string();
        assert!(err.contains("authenticate"), "{err}");
    }

    /// `surfaces = "all"` is the every-surface sentinel; unknown surface
    /// names are load errors (they used to be silently skipped).
    #[test]
    fn surfaces_all_sentinel_and_strict_entries() {
        let lua = Lua::new();

        let m = strategy_method(&lua, Some("hooks.auth.sso"), true);
        m.set("surfaces", "all").unwrap();
        methods_config(&lua, vec![m]).unwrap();

        let m = strategy_method(&lua, Some("hooks.auth.sso"), true);
        m.set("surfaces", "grpc").unwrap();
        let err = methods_config(&lua, vec![m]).unwrap_err().to_string();
        assert!(err.contains("must be \"all\" or a list"), "{err}");

        let m = strategy_method(&lua, Some("hooks.auth.sso"), true);
        let list = lua.create_table().unwrap();
        list.set(1, "gprc").unwrap();
        m.set("surfaces", list).unwrap();
        let err = methods_config(&lua, vec![m]).unwrap_err().to_string();
        assert!(err.contains("unknown surface \"gprc\""), "{err}");
    }

    #[test]
    fn validate_rejects_old_top_level_keys() {
        let lua = Lua::new();
        // `disable_local` / `strategies` were removed in the methods migration.
        let err = auth_config(&lua, |a| {
            a.set("disable_local", true).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("disable_local"), "{err}");
    }

    #[test]
    fn validate_rejects_unknown_method_key_per_type() {
        let lua = Lua::new();
        // `surfaces` is not valid on password_login.
        let err = auth_config(&lua, |a| {
            let methods = lua.create_table().unwrap();
            let m = lua.create_table().unwrap();
            m.set("type", "password_login").unwrap();
            let s = lua.create_table().unwrap();
            s.set(1, "grpc").unwrap();
            m.set("surfaces", s).unwrap();
            methods.set(1, m).unwrap();
            a.set("methods", methods).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("surfaces"), "{err}");
    }

    #[test]
    fn validate_rejects_unknown_method_type() {
        let lua = Lua::new();
        let err = auth_config(&lua, |a| {
            let methods = lua.create_table().unwrap();
            let m = lua.create_table().unwrap();
            m.set("type", "password").unwrap(); // typo for password_login
            methods.set(1, m).unwrap();
            a.set("methods", methods).unwrap();
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("password"), "{err}");
    }

    #[test]
    fn validate_accepts_default_method_shape() {
        let lua = Lua::new();
        let result = auth_config(&lua, |a| {
            a.set("enabled", true).unwrap();
            a.set("token_expiry", 3600).unwrap();
            let methods = lua.create_table().unwrap();
            let m = lua.create_table().unwrap();
            m.set("type", "strategy").unwrap();
            m.set("name", "api-key").unwrap();
            m.set("authenticate", "hooks.auth.api_key").unwrap();
            let act = lua.create_table().unwrap();
            act.set("header", "x-api-key").unwrap();
            m.set("activates_on", act).unwrap();
            methods.set(1, m).unwrap();
            a.set("methods", methods).unwrap();
        });
        assert!(result.is_ok(), "{result:?}");
    }
}
