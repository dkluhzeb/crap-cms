//! Context types passed to auth/access hook callbacks. Each struct
//! carries `Serialize` (so the hook runner builds it once and Lua sees
//! it via `lua.to_value()`) and `LuaAnnotation` (so the Lua-side type
//! lives next to the Rust struct that produces it).

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::typegen::lua::LuaAnnotation;

/// Context passed to `strategy`-type auth `authenticate` hooks.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.AuthStrategyContext")]
pub struct AuthStrategyContext<'a> {
    /// Request headers (lowercase keys).
    #[lua(ty = "table<string, string>")]
    pub headers: &'a HashMap<String, String>,
    /// Auth collection slug.
    pub collection: &'a str,
    /// The submitted login identifier (email/username), when the strategy was
    /// reached via a password-style login. `nil` for header/token flows (OAuth).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub email: Option<&'a str>,
    /// The submitted plaintext password, for strategies that verify credentials
    /// against an external system (LDAP, a remote API). `nil` for header/token
    /// flows. **Sensitive** — only your strategy hook receives it; never log it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub password: Option<&'a str>,
    /// The client's remote IP address, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub remote_addr: Option<&'a str>,
    /// Per-config options from the strategy's `authenticate` `{ ref, options }`
    /// table; `nil` when configured as a bare ref string.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub options: Option<&'a Value>,
}

/// Rust-side request bundle for `HookRunner::run_auth_strategy`. Each login
/// surface (gRPC, admin form, OAuth callback) builds one; the runner converts
/// it into the Lua-facing [`AuthStrategyContext`].
pub struct AuthStrategyInput<'a> {
    pub collection: &'a str,
    pub headers: &'a HashMap<String, String>,
    pub email: Option<&'a str>,
    pub password: Option<&'a str>,
    pub remote_addr: Option<&'a str>,
}
