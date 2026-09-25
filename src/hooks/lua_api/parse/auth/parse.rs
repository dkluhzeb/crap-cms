//! Parsing of a validated `auth` table into the typed [`Auth`] config.

use mlua::{Table, Value};

use crate::{
    core::collection::{Activation, Auth, AuthMethod, MfaMode, Surface, SurfaceSet},
    hooks::lua_api::parse::helpers::{get_bool, get_optional_hook_ref, get_table},
};

/// The `mfa_exempt_callbacks` names (validated by [`validate_exempt_callbacks`]).
fn parse_exempt_callbacks(tbl: &Table) -> Vec<String> {
    get_table(tbl, "mfa_exempt_callbacks")
        .map(|t| t.sequence_values::<String>().flatten().collect())
        .unwrap_or_default()
}

pub(in crate::hooks::lua_api::parse) fn parse_collection_auth(config: &Table) -> Option<Auth> {
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
            // Validated by `validate_token_expiry`; unset = the global default.
            let token_expiry = tbl.get::<Option<u64>>("token_expiry").ok().flatten();
            // `get_bool`, not `get::<bool>`: mlua reads a missing key as
            // `false`, which made every `auth = { methods = {...} }` table
            // without an explicit `enabled = true` parse as disabled.
            let enabled = get_bool(&tbl, "enabled", true).unwrap_or(true);
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
                Some("custom") => MfaMode::Custom,
                Some("totp") => MfaMode::Totp,
                // Unknown strings were rejected by `validate_method_keys`
                // before this runs; anything else (`false`, absent) is Off.
                _ => MfaMode::Off,
            },
            mfa_when: get_optional_hook_ref(tbl, "mfa_when", "password_login method")
                .ok()
                .flatten()
                .filter(|h| !h.reference().is_empty()),
            mfa_deliver: get_optional_hook_ref(tbl, "mfa_deliver", "password_login method")
                .ok()
                .flatten()
                .filter(|h| !h.reference().is_empty()),
            mfa_exempt_callbacks: parse_exempt_callbacks(tbl),
            verify_email: get_bool(tbl, "verify_email", false).unwrap_or(false),
            // Same nil-is-false trap as `enabled`: a missing key must mean
            // the documented default (`true`), not "disabled".
            forgot_password: get_bool(tbl, "forgot_password", true).unwrap_or(true),
        }),
        "bearer" => Some(AuthMethod::Bearer {
            surfaces: parse_surfaces(tbl).unwrap_or_else(SurfaceSet::all),
        }),
        "session_cookie" => Some(AuthMethod::SessionCookie {
            surfaces: parse_surfaces(tbl).unwrap_or_else(SurfaceSet::admin_only),
        }),
        "strategy" => {
            let name = tbl.get::<String>("name").unwrap_or_default();
            let authenticate = match get_optional_hook_ref(tbl, "authenticate", "auth strategy") {
                Ok(Some(h)) if !h.reference().is_empty() => h,
                _ => return None,
            };
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
    // `surfaces = "all"` — every current AND future surface. Spelled as a
    // sentinel so existing configs aren't silently excluded from a third
    // surface added later. (Strict entry validation lives in
    // `validate_surfaces`, which runs first; this parser stays lenient.)
    if let Ok(s) = tbl.get::<String>("surfaces")
        && s == "all"
    {
        return Some(SurfaceSet::all());
    }

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

pub(super) fn parse_activation(tbl: &Table) -> Option<Activation> {
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
    use serde_json::json;

    use crate::core::HookRef;

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

    /// `mfa = "totp"` parses into the typed mode.
    #[test]
    fn parses_totp_mode() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "password_login").unwrap();
        m.set("mfa", "totp").unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();

        let auth = parse_collection_auth(&tbl).unwrap();
        let AuthMethod::PasswordLogin { mfa, .. } = &auth.methods[0] else {
            panic!("expected PasswordLogin");
        };
        assert_eq!(*mfa, MfaMode::Totp);
    }

    /// `mfa = "custom"` + `mfa_deliver` parse into the typed pair (the
    /// startup validator enforces they arrive together).
    #[test]
    fn parses_custom_mfa_with_deliver_hook() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "password_login").unwrap();
        m.set("mfa", "custom").unwrap();
        m.set("mfa_deliver", "hooks.mfa.send_sms").unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();

        let auth = parse_collection_auth(&tbl).unwrap();
        let AuthMethod::PasswordLogin {
            mfa, mfa_deliver, ..
        } = &auth.methods[0]
        else {
            panic!("expected PasswordLogin");
        };
        assert_eq!(*mfa, MfaMode::Custom);
        assert_eq!(
            mfa_deliver.as_ref().map(HookRef::reference),
            Some("hooks.mfa.send_sms")
        );
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
                assert_eq!(authenticate.reference(), "hooks.auth.api_key");
                assert!(
                    matches!(activates_on, Activation::Header { header } if header == "x-api-key")
                );
                assert_eq!(surfaces, &SurfaceSet::grpc_only());
            }
            other => panic!("expected Strategy, got {other:?}"),
        }
    }

    /// A strategy `authenticate` declared as `{ ref, options }` parses to a
    /// `HookRef` carrying the options (exposed to the strategy as `ctx.options`).
    #[test]
    fn parse_strategy_authenticate_with_options() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "strategy").unwrap();
        m.set("name", "api-key").unwrap();
        let auth_ref = lua.create_table().unwrap();
        auth_ref.set("ref", "hooks.auth.api_key").unwrap();
        let opts = lua.create_table().unwrap();
        opts.set("header", "x-api-key").unwrap();
        auth_ref.set("options", opts).unwrap();
        m.set("authenticate", auth_ref).unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();

        let auth = parse_collection_auth(&tbl).unwrap();
        match &auth.methods[0] {
            AuthMethod::Strategy { authenticate, .. } => {
                assert_eq!(authenticate.reference(), "hooks.auth.api_key");
                assert_eq!(
                    authenticate.options().and_then(|o| o.get("header")),
                    Some(&json!("x-api-key"))
                );
            }
            other => panic!("expected Strategy, got {other:?}"),
        }
    }

    /// Parser-layer fallback only: `validate_auth_keys` rejects a strategy
    /// without `activates_on` before `parse_collection_auth` ever runs, so
    /// this default is unreachable from a real definition file.
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

    /// The parser maps the sentinel to `SurfaceSet::all()`.
    #[test]
    fn parse_surfaces_all_sentinel() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let auth_tbl = lua.create_table().unwrap();
        let methods = lua.create_table().unwrap();
        let m = lua.create_table().unwrap();
        m.set("type", "bearer").unwrap();
        m.set("surfaces", "all").unwrap();
        methods.set(1, m).unwrap();
        auth_tbl.set("methods", methods).unwrap();
        tbl.set("auth", auth_tbl).unwrap();

        let auth = parse_collection_auth(&tbl).unwrap();
        match &auth.methods[0] {
            AuthMethod::Bearer { surfaces } => {
                assert!(surfaces.contains(Surface::Admin));
                assert!(surfaces.contains(Surface::Grpc));
            }
            other => panic!("expected Bearer, got {other:?}"),
        }
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
