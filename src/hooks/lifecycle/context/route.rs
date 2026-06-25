//! Context passed to a custom HTTP route handler (`crap.routes.register`).
//!
//! Mirrors the [`AuthStrategyContext`](super::strategy::AuthStrategyContext)
//! pattern: `Serialize` so the route dispatcher builds it once and Lua sees it
//! via `lua.to_value()`, and `LuaAnnotation` so the Lua-side type lives next to
//! the Rust struct that produces it.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::core::Document;
use crate::typegen::lua::LuaAnnotation;

/// Owned request bundle the route dispatcher builds and hands to
/// `HookRunner::run_route_handler`. Owned (not borrowed) so it can cross the
/// `spawn_blocking` boundary; the runner borrows from it to build the Lua-facing
/// [`RouteContext`].
pub struct RouteHandlerInput {
    pub method: String,
    pub path: String,
    pub params: HashMap<String, String>,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub cookies: HashMap<String, String>,
    pub body: Option<String>,
    pub json: Option<Value>,
    pub form: Option<HashMap<String, String>>,
    pub user: Option<Document>,
    pub collection: Option<String>,
    pub ip: String,
    pub ui_locale: Option<String>,
    pub options: Option<Value>,
}

impl RouteHandlerInput {
    /// Borrow the input as the Lua-facing [`RouteContext`].
    #[must_use]
    pub fn context(&self) -> RouteContext<'_> {
        RouteContext {
            method: &self.method,
            path: &self.path,
            params: &self.params,
            query: &self.query,
            headers: &self.headers,
            cookies: &self.cookies,
            body: self.body.as_deref(),
            json: self.json.as_ref(),
            form: self.form.as_ref(),
            user: self.user.as_ref(),
            collection: self.collection.as_deref(),
            ip: &self.ip,
            ui_locale: self.ui_locale.as_deref(),
            options: self.options.as_ref(),
        }
    }
}

/// The `ctx` table a custom route handler receives. The dispatcher pre-parses
/// cookies, query, and (by content-type) the body, so the handler reads them
/// directly instead of plumbing the raw request itself.
#[derive(Serialize, LuaAnnotation)]
#[lua(class = "crap.RouteContext")]
pub struct RouteContext<'a> {
    /// HTTP method, upper-cased (`"GET"`, `"POST"`, …).
    pub method: &'a str,
    /// The matched route path.
    pub path: &'a str,
    /// Path parameters (`/items/{id}` → `ctx.params.id`).
    #[lua(ty = "table<string, string>")]
    pub params: &'a HashMap<String, String>,
    /// Parsed query string parameters.
    #[lua(ty = "table<string, string>")]
    pub query: &'a HashMap<String, String>,
    /// Request headers (lowercase keys).
    #[lua(ty = "table<string, string>")]
    pub headers: &'a HashMap<String, String>,
    /// Parsed request cookies (`ctx.cookies.crap_session`).
    #[lua(ty = "table<string, string>")]
    pub cookies: &'a HashMap<String, String>,
    /// Raw request body as a string, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub body: Option<&'a str>,
    /// Parsed body when `Content-Type` is JSON; `nil` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub json: Option<&'a Value>,
    /// Parsed body for `application/x-www-form-urlencoded`; `nil` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table<string, string>", optional)]
    pub form: Option<&'a HashMap<String, String>>,
    /// The authenticated user document, when the request carried a valid
    /// session/bearer; `nil` for anonymous requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "crap.AuthUser", optional)]
    pub user: Option<&'a Document>,
    /// The auth collection the user belongs to; `nil` when anonymous.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub collection: Option<&'a str>,
    /// The client's remote IP (spoof-resistant; honors trusted proxies).
    pub ip: &'a str,
    /// Admin UI locale, when the request originated from the admin UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(optional)]
    pub ui_locale: Option<&'a str>,
    /// Per-route options from `crap.routes.register({ options = … })`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[lua(ty = "table", optional)]
    pub options: Option<&'a Value>,
}
