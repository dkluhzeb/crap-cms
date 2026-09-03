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

use super::helpers::{deny_unknown_keys, get_bool, get_optional_hook_ref, get_table};

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
            mfa_deliver.as_ref().map(crate::core::HookRef::reference),
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
                    Some(&serde_json::json!("x-api-key"))
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

    fn auth_config(lua: &Lua, build: impl FnOnce(&Table)) -> Result<()> {
        let config = lua.create_table().unwrap();
        let auth = lua.create_table().unwrap();
        build(&auth);
        config.set("auth", auth).unwrap();
        validate_auth_keys(&config)
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
