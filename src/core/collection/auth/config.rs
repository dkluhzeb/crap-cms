//! Top-level collection auth configuration and its borrowed method views.

use serde::{Deserialize, Serialize};

use crate::{
    core::{
        HookRef,
        collection::{Activation, AuthMethod, MfaMode, PasswordLoginBuilder, Surface, SurfaceSet},
    },
    typegen::lua::LuaAnnotation,
};

/// Top-level authentication configuration for a collection.
///
/// `enabled = true` + non-empty `methods` makes this an auth
/// collection — provisions `_password_hash` and friends in the
/// schema, registers it as a target for `Login` / `Me` / etc.
///
/// `methods` is required (no implicit defaults). Use
/// `crap.auth.default_methods()` from Lua for the common
/// password+bearer+cookie set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.Auth")]
pub struct Auth {
    /// Enable auth for this collection. Required true when `methods` is non-empty.
    #[lua(optional)]
    pub enabled: bool,
    /// Session token lifetime in seconds. Unset, the global `[auth] token_expiry` applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub token_expiry: Option<u64>,
    /// Ordered list of auth methods. Use `crap.auth.default_methods()` for the standard set or `crap.auth.with_defaults({...})` to extend it.
    #[serde(default)]
    #[lua(optional, ty = "crap.AuthMethod[]")]
    pub methods: Vec<AuthMethod>,
}

impl Auth {
    /// Create a new auth config with the given enabled flag.
    /// Methods must be set separately.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }

    /// The session token lifetime, in seconds: the collection's own
    /// `token_expiry`, or `global` — the `[auth] token_expiry` default — when
    /// the collection sets none.
    #[must_use]
    pub fn token_lifetime(&self, global: u64) -> u64 {
        self.token_expiry.unwrap_or(global)
    }

    /// The standard default method set:
    ///   1. `password_login` (no MFA, no `verify_email`, `forgot_password = true`)
    ///   2. `bearer` (all surfaces)
    ///   3. `session_cookie` (admin only)
    ///
    /// Mirrors `crap.auth.default_methods()` on the Lua side.
    #[must_use]
    pub fn default_methods() -> Vec<AuthMethod> {
        vec![
            AuthMethod::PasswordLogin {
                mfa: MfaMode::Off,
                mfa_when: None,
                mfa_deliver: None,
                mfa_exempt_callbacks: Vec::new(),
                verify_email: false,
                forgot_password: true,
            },
            AuthMethod::Bearer {
                surfaces: SurfaceSet::all(),
            },
            AuthMethod::SessionCookie {
                surfaces: SurfaceSet::admin_only(),
            },
        ]
    }

    /// Find the `password_login` method's configuration, if
    /// present. `None` means this collection does not accept
    /// email/password login at all (the `Login` RPC returns
    /// `INVALID_ARGUMENT` for this collection).
    #[must_use]
    pub fn password_login(&self) -> Option<PasswordLoginCfg> {
        self.methods.iter().find_map(|m| match m {
            AuthMethod::PasswordLogin {
                mfa,
                mfa_when: _,
                mfa_deliver: _,
                mfa_exempt_callbacks: _,
                verify_email,
                forgot_password,
            } => Some(PasswordLoginCfg {
                mfa: *mfa,
                verify_email: *verify_email,
                forgot_password: *forgot_password,
            }),
            _ => None,
        })
    }

    /// True iff the `bearer` method is present and covers `surface`.
    #[must_use]
    pub fn accepts_bearer(&self, surface: Surface) -> bool {
        self.methods.iter().any(|m| match m {
            AuthMethod::Bearer { surfaces } => surfaces.contains(surface),
            _ => false,
        })
    }

    /// True iff the `session_cookie` method is present and covers `surface`.
    #[must_use]
    pub fn accepts_session_cookie(&self, surface: Surface) -> bool {
        self.methods.iter().any(|m| match m {
            AuthMethod::SessionCookie { surfaces } => surfaces.contains(surface),
            _ => false,
        })
    }

    /// True iff this collection's `password_login` method has
    /// `verify_email = true` (or there's no `password_login` at all
    /// when called in contexts where "must verify" is the default —
    /// this returns false for missing `password_login`).
    #[must_use]
    pub fn requires_verify_email(&self) -> bool {
        self.password_login().is_some_and(|c| c.verify_email)
    }

    /// True iff this collection's `password_login` method has
    /// `forgot_password = true`. Missing `password_login` → false
    /// (the flow needs Login to even start).
    #[must_use]
    pub fn forgot_password_enabled(&self) -> bool {
        self.password_login().is_some_and(|c| c.forgot_password)
    }

    /// True iff this collection has a `password_login` method —
    /// i.e. accepts email + password via the `Login` flow. Inverse
    /// of the old `disable_local` flag.
    #[must_use]
    pub fn password_login_enabled(&self) -> bool {
        self.password_login().is_some()
    }

    /// Get the configured MFA mode (or `Off` when no `password_login`).
    #[must_use]
    pub fn mfa(&self) -> MfaMode {
        self.password_login().map_or(MfaMode::Off, |c| c.mfa)
    }

    /// The `password_login` method's `mfa_when` gate hook, if configured.
    #[must_use]
    pub fn mfa_when(&self) -> Option<&HookRef> {
        self.methods.iter().find_map(|m| match m {
            AuthMethod::PasswordLogin { mfa_when, .. } => mfa_when.as_ref(),
            _ => None,
        })
    }

    /// The `password_login` method's `mfa_deliver` hook, if configured.
    #[must_use]
    pub fn mfa_deliver(&self) -> Option<&HookRef> {
        self.methods.iter().find_map(|m| match m {
            AuthMethod::PasswordLogin { mfa_deliver, .. } => mfa_deliver.as_ref(),
            _ => None,
        })
    }

    /// True iff the `password_login` method exempts the auth callback `name`
    /// from the MFA step (its identity provider enforces a second factor).
    #[must_use]
    pub fn mfa_exempts_callback(&self, name: &str) -> bool {
        self.methods.iter().any(|m| match m {
            AuthMethod::PasswordLogin {
                mfa_exempt_callbacks,
                ..
            } => mfa_exempt_callbacks.iter().any(|n| n == name),
            _ => false,
        })
    }

    /// True iff this collection has at least one `strategy` method.
    #[must_use]
    pub fn has_strategies(&self) -> bool {
        self.strategies().next().is_some()
    }

    // ── Fluent builders for tests / programmatic construction ────────

    /// `Auth { enabled = true, methods = default_methods() }` —
    /// the standard "password + bearer + cookie" shape.
    #[must_use]
    pub fn enabled() -> Self {
        Self {
            enabled: true,
            token_expiry: None,
            methods: Self::default_methods(),
        }
    }

    /// Re-build the `password_login` method via the given builder
    /// closure. The closure receives a [`PasswordLoginBuilder`]
    /// seeded with the current method's settings, and the returned
    /// builder replaces the existing entry.
    ///
    /// No-op when no `password_login` method is present — prefer
    /// chaining on [`AuthMethod::password_login_builder`] when you
    /// want compile-time guarantees the method exists.
    #[must_use]
    pub fn map_password_login(
        mut self,
        f: impl FnOnce(PasswordLoginBuilder) -> PasswordLoginBuilder,
    ) -> Self {
        if let Some(idx) = self
            .methods
            .iter()
            .position(|m| matches!(m, AuthMethod::PasswordLogin { .. }))
            && let AuthMethod::PasswordLogin {
                mfa,
                ref mfa_when,
                ref mfa_deliver,
                ref mfa_exempt_callbacks,
                verify_email,
                forgot_password,
            } = self.methods[idx]
        {
            let seed = PasswordLoginBuilder {
                mfa,
                mfa_when: mfa_when.clone(),
                mfa_deliver: mfa_deliver.clone(),
                mfa_exempt_callbacks: mfa_exempt_callbacks.clone(),
                verify_email,
                forgot_password,
            };
            self.methods[idx] = f(seed).build();
        }
        self
    }

    /// Iterate all `strategy` methods on this collection in their
    /// declared `methods`-list order.
    ///
    /// Order within a collection is deterministic. Order *across*
    /// collections is `HashMap` iteration order (see
    /// [`service::auth::evaluate`](crate::service::auth::evaluate)
    /// for the cross-collection caveat) — callers that walk
    /// strategies of all collections in succession get a stable
    /// ordering only per-collection.
    pub fn strategies(&self) -> impl Iterator<Item = StrategyCfg<'_>> {
        self.methods.iter().filter_map(|m| match m {
            AuthMethod::Strategy {
                name,
                authenticate,
                activates_on,
                surfaces,
            } => Some(StrategyCfg {
                name,
                authenticate,
                activates_on,
                surfaces,
            }),
            _ => None,
        })
    }
}

/// Borrowed view of a `password_login` method's config.
#[derive(Debug, Clone, Copy)]
pub struct PasswordLoginCfg {
    pub mfa: MfaMode,
    pub verify_email: bool,
    pub forgot_password: bool,
}

/// Borrowed view of a `strategy` method's config.
#[derive(Debug, Clone, Copy)]
pub struct StrategyCfg<'a> {
    pub name: &'a str,
    pub authenticate: &'a HookRef,
    pub activates_on: &'a Activation,
    pub surfaces: &'a SurfaceSet,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── shape + defaults ─────────────────────────────────────────────────

    #[test]
    fn collection_auth_defaults_disabled_with_empty_methods() {
        let auth = Auth::default();
        assert!(!auth.enabled);
        assert_eq!(auth.token_expiry, None, "unset: the global default applies");
        assert!(auth.methods.is_empty());
    }

    /// Regression: a collection without its own `token_expiry` parsed as
    /// 7200, so the global `[auth] token_expiry` it documents as the default
    /// never applied. Unset, the global value is the lifetime; set, the
    /// collection's own wins.
    #[test]
    fn token_lifetime_falls_back_to_the_global_default() {
        let inherits = Auth::enabled();
        assert_eq!(inherits.token_lifetime(86_400), 86_400);

        let overrides = Auth {
            token_expiry: Some(600),
            ..Auth::enabled()
        };
        assert_eq!(overrides.token_lifetime(86_400), 600);
    }

    #[test]
    fn default_methods_has_password_bearer_cookie_in_order() {
        let methods = Auth::default_methods();
        assert_eq!(methods.len(), 3);
        assert!(matches!(methods[0], AuthMethod::PasswordLogin { .. }));
        assert!(matches!(methods[1], AuthMethod::Bearer { .. }));
        assert!(matches!(methods[2], AuthMethod::SessionCookie { .. }));
    }

    #[test]
    fn default_methods_bearer_covers_all_surfaces() {
        let auth = Auth {
            enabled: true,
            methods: Auth::default_methods(),
            ..Default::default()
        };
        assert!(auth.accepts_bearer(Surface::Admin));
        assert!(auth.accepts_bearer(Surface::Grpc));
    }

    #[test]
    fn default_methods_cookie_covers_admin_only() {
        let auth = Auth {
            enabled: true,
            methods: Auth::default_methods(),
            ..Default::default()
        };
        assert!(auth.accepts_session_cookie(Surface::Admin));
        assert!(!auth.accepts_session_cookie(Surface::Grpc));
    }

    #[test]
    fn password_login_accessor_extracts_config() {
        let auth = Auth {
            enabled: true,
            methods: vec![AuthMethod::PasswordLogin {
                mfa: MfaMode::Email,
                mfa_when: None,
                mfa_deliver: None,
                mfa_exempt_callbacks: Vec::new(),
                verify_email: true,
                forgot_password: false,
            }],
            ..Default::default()
        };
        let cfg = auth.password_login().expect("present");
        assert_eq!(cfg.mfa, MfaMode::Email);
        assert!(cfg.verify_email);
        assert!(!cfg.forgot_password);
    }

    /// Only the callbacks the `password_login` method lists skip MFA.
    #[test]
    fn mfa_exempts_only_the_listed_callbacks() {
        let auth = Auth::enabled().map_password_login(|b| {
            b.mfa(MfaMode::Totp)
                .mfa_exempt_callbacks(vec!["okta".to_string()])
        });

        assert!(auth.mfa_exempts_callback("okta"));
        assert!(!auth.mfa_exempts_callback("google"));
        assert!(!Auth::enabled().mfa_exempts_callback("okta"));
    }

    #[test]
    fn password_login_absent_when_method_not_listed() {
        let auth = Auth {
            enabled: true,
            methods: vec![AuthMethod::Bearer {
                surfaces: SurfaceSet::all(),
            }],
            ..Default::default()
        };
        assert!(auth.password_login().is_none());
    }

    #[test]
    fn strategies_iter_yields_only_strategy_variants() {
        let auth = Auth {
            enabled: true,
            methods: vec![
                AuthMethod::Bearer {
                    surfaces: SurfaceSet::all(),
                },
                AuthMethod::Strategy {
                    name: "api-key".into(),
                    authenticate: "hooks.auth.api_key".into(),
                    activates_on: Activation::Header {
                        header: "x-api-key".into(),
                    },
                    surfaces: SurfaceSet::grpc_only(),
                },
                AuthMethod::Strategy {
                    name: "sso".into(),
                    authenticate: "hooks.auth.sso".into(),
                    activates_on: Activation::always(),
                    surfaces: SurfaceSet::admin_only(),
                },
            ],
            ..Default::default()
        };
        let names: Vec<&str> = auth.strategies().map(|s| s.name).collect();
        assert_eq!(names, vec!["api-key", "sso"]);
    }

    // ── serde round-trips ────────────────────────────────────────────────

    #[test]
    fn json_round_trip_default_methods() {
        let auth = Auth {
            enabled: true,
            methods: Auth::default_methods(),
            ..Default::default()
        };
        let json = serde_json::to_string(&auth).unwrap();
        let back: Auth = serde_json::from_str(&json).unwrap();
        assert_eq!(back.methods.len(), 3);
    }
}
