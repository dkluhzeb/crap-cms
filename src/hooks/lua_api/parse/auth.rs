//! Parsing functions for collection auth configuration.
//!
//! Lua-side shape (new):
//! ```lua
//! auth = {
//!     enabled = true,
//!     token_expiry = 7200,                  -- optional
//!     methods = {                           -- required when enabled
//!         { type = "password_login", mfa = "email", verify_email = true },
//!         { type = "bearer", surfaces = {"grpc", "admin"} },
//!         { type = "session_cookie", surfaces = {"admin"} },
//!         { type = "strategy",
//!           name = "api-key",
//!           authenticate = "hooks.auth.api_key",
//!           activates_on = { header = "x-api-key" },
//!           surfaces = {"grpc"} },
//!     },
//! }
//! ```

use anyhow::{Result, bail};
use mlua::{Table, Value};

use crate::core::collection::{Activation, Auth, AuthMethod, MfaMode, Surface, SurfaceSet};

use super::helpers::{deny_unknown_keys, get_table};

/// Keys accepted on the top-level `auth = { ... }` table.
const AUTH_KEYS: &[&str] = &["enabled", "token_expiry", "methods"];

/// Reject unknown keys in the `auth` sub-table, its `methods` entries (validated
/// per method `type`), and each strategy's `activates_on` discriminator. A typo'd
/// or removed key (the old `disable_local` / `strategies`) fails loudly instead of
/// being silently ignored. `auth = true`/`false` (boolean form) is skipped.
pub(super) fn validate_auth_keys(config: &Table) -> Result<()> {
    let Ok(auth_tbl) = get_table(config, "auth") else {
        return Ok(());
    };

    deny_unknown_keys(&auth_tbl, "auth", AUTH_KEYS)?;

    let Ok(methods_tbl) = get_table(&auth_tbl, "methods") else {
        return Ok(());
    };

    for method in methods_tbl.sequence_values::<Table>().flatten() {
        validate_method_keys(&method)?;
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

    let allowed: &[&str] = match ty.as_str() {
        "password_login" => &["type", "mfa", "verify_email", "forgot_password"],
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

    Ok(())
}

pub(super) fn parse_collection_auth(config: &Table) -> Option<Auth> {
    let val: Value = config.get("auth").ok()?;

    match val {
        Value::Boolean(true) => {
            // Shorthand `auth = true` — enabled with the default
            // method set (password_login + bearer + session_cookie).
            let mut auth = Auth::new(true);
            auth.methods = Auth::default_methods();
            Some(auth)
        }
        Value::Table(tbl) => {
            let token_expiry = tbl.get::<u64>("token_expiry").unwrap_or(7200);
            let enabled = tbl.get::<bool>("enabled").unwrap_or(true);
            let mut methods = parse_methods(&tbl);

            // If `enabled = true` but no `methods` listed, fall back
            // to the default set. Lets `auth = { enabled = true }`
            // keep working as shorthand.
            if enabled && methods.is_empty() {
                methods = Auth::default_methods();
            }

            let mut auth = Auth::new(enabled);
            auth.token_expiry = token_expiry;
            auth.methods = methods;

            Some(auth)
        }
        _ => None,
    }
}

fn parse_methods(tbl: &Table) -> Vec<AuthMethod> {
    let Ok(methods_tbl) = get_table(tbl, "methods") else {
        return Vec::new();
    };

    methods_tbl
        .sequence_values::<Table>()
        .flatten()
        .filter_map(|m| parse_method(&m))
        .collect()
}

fn parse_method(tbl: &Table) -> Option<AuthMethod> {
    let ty = tbl.get::<String>("type").ok()?;

    match ty.as_str() {
        "password_login" => Some(AuthMethod::PasswordLogin {
            mfa: match tbl.get::<String>("mfa").ok().as_deref() {
                Some("email") => MfaMode::Email,
                _ => MfaMode::Off,
            },
            verify_email: tbl.get::<bool>("verify_email").unwrap_or(false),
            forgot_password: tbl.get::<bool>("forgot_password").unwrap_or(true),
        }),
        "bearer" => Some(AuthMethod::Bearer {
            surfaces: parse_surfaces(tbl).unwrap_or_else(SurfaceSet::all),
        }),
        "session_cookie" => Some(AuthMethod::SessionCookie {
            surfaces: parse_surfaces(tbl).unwrap_or_else(SurfaceSet::admin_only),
        }),
        "strategy" => {
            let name = tbl.get::<String>("name").unwrap_or_default();
            let authenticate = tbl.get::<String>("authenticate").unwrap_or_default();
            if authenticate.is_empty() {
                return None;
            }
            let activates_on = parse_activation(tbl).unwrap_or(Activation::always());
            Some(AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces: parse_surfaces(tbl).unwrap_or_else(SurfaceSet::admin_only),
            })
        }
        _ => None,
    }
}

fn parse_surfaces(tbl: &Table) -> Option<SurfaceSet> {
    let surfaces_tbl: Table = tbl.get("surfaces").ok()?;
    let mut out = Vec::new();
    for s in surfaces_tbl.sequence_values::<String>().flatten() {
        match s.as_str() {
            "admin" => out.push(Surface::Admin),
            "grpc" => out.push(Surface::Grpc),
            _ => {}
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(SurfaceSet::from_list(out))
    }
}

fn parse_activation(tbl: &Table) -> Option<Activation> {
    let act_tbl: Table = tbl.get("activates_on").ok()?;
    if let Ok(header) = act_tbl.get::<String>("header")
        && !header.is_empty()
    {
        return Some(Activation::Header { header });
    }
    if act_tbl.get::<bool>("always").unwrap_or(false) {
        return Some(Activation::always());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    #[test]
    fn parse_auth_true_yields_empty_methods() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("auth", true).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        assert!(auth.enabled);
        assert_eq!(auth.methods.len(), 3); // shorthand auth=true populates default_methods
    }

    #[test]
    fn parse_auth_false_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("auth", false).unwrap();
        assert!(parse_collection_auth(&tbl).is_none());
    }

    #[test]
    fn parse_methods_default_three() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m1 = lua.create_table().unwrap();
        m1.set("type", "password_login").unwrap();
        methods.set(1, m1).unwrap();
        let m2 = lua.create_table().unwrap();
        m2.set("type", "bearer").unwrap();
        methods.set(2, m2).unwrap();
        let m3 = lua.create_table().unwrap();
        m3.set("type", "session_cookie").unwrap();
        methods.set(3, m3).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        assert_eq!(auth.methods.len(), 3);
        assert!(matches!(auth.methods[0], AuthMethod::PasswordLogin { .. }));
        assert!(matches!(auth.methods[1], AuthMethod::Bearer { .. }));
        assert!(matches!(auth.methods[2], AuthMethod::SessionCookie { .. }));
    }

    #[test]
    fn parse_strategy_with_header_activation() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "strategy").unwrap();
        m.set("name", "api-key").unwrap();
        m.set("authenticate", "hooks.auth.api_key").unwrap();
        let act = lua.create_table().unwrap();
        act.set("header", "x-api-key").unwrap();
        m.set("activates_on", act).unwrap();
        let surfaces = lua.create_table().unwrap();
        surfaces.set(1, "grpc").unwrap();
        m.set("surfaces", surfaces).unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        assert_eq!(auth.methods.len(), 1);
        match &auth.methods[0] {
            AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces,
            } => {
                assert_eq!(name, "api-key");
                assert_eq!(authenticate, "hooks.auth.api_key");
                assert!(
                    matches!(activates_on, Activation::Header { header } if header == "x-api-key")
                );
                assert_eq!(surfaces, &SurfaceSet::grpc_only());
            }
            other => panic!("expected Strategy, got {other:?}"),
        }
    }

    #[test]
    fn parse_strategy_without_activates_on_defaults_to_always() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "strategy").unwrap();
        m.set("name", "any").unwrap();
        m.set("authenticate", "hooks.auth.any").unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        match &auth.methods[0] {
            AuthMethod::Strategy { activates_on, .. } => {
                assert!(matches!(activates_on, Activation::Always { .. }));
            }
            _ => panic!("expected Strategy"),
        }
    }

    fn auth_config(lua: &Lua, build: impl FnOnce(&Table)) -> Result<()> {
        let config = lua.create_table().unwrap();
        let auth = lua.create_table().unwrap();
        build(&auth);
        config.set("auth", auth).unwrap();
        validate_auth_keys(&config)
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

    #[test]
    fn parse_strategy_missing_authenticate_skipped() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "strategy").unwrap();
        m.set("name", "incomplete").unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();
        let auth = parse_collection_auth(&tbl).unwrap();
        // Bad strategy gets dropped; methods list ends up empty →
        // fallback populates default_methods (no Strategy entry).
        assert!(
            auth.strategies().next().is_none(),
            "incomplete strategy must not appear in methods"
        );
    }
}
