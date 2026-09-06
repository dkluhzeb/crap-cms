//! `HookRunner` methods for custom HTTP routes — extraction of the registry
//! declared via `crap.routes.register`. Execution (`run_route_handler`) is added
//! alongside once the route context / response envelope types exist.

use std::{cell::RefCell, rc::Rc};

use anyhow::{Result, anyhow};
use mlua::{Lua, LuaSerdeExt as _, Result as LuaResult, Table, Value};
use tracing::warn;

use crate::{
    admin::custom_routes::{
        RouteAccess, RouteBody, RouteCookie, RouteDefinition, RouteRateLimit, RouteResponse,
    },
    core::HookRef,
    db::DbPool,
    hooks::{
        HookRunner,
        lifecycle::{
            LuaCrudInfra, RouteHandlerInput, execution::resolve_hook_function,
            types::TxContextGuard,
        },
        lua_api,
        lua_api::parse::deny_unknown_keys,
        lua_api::routes::ROUTES_KEY,
    },
    service::{EventQueue, ServiceContext, flush_queue},
};

impl HookRunner {
    /// Read every entry registered via `crap.routes.register(...)` and convert
    /// to the typed [`RouteDefinition`] list. Called once at admin-server
    /// startup to mount the routes.
    ///
    /// Returns an empty Vec if Lua isn't available or no routes are registered.
    /// A malformed entry is dropped (logged) rather than mounted half-decoded.
    #[must_use]
    pub fn extract_routes(&self) -> Vec<RouteDefinition> {
        let Ok(lua) = self.pool.acquire() else {
            return Vec::new();
        };

        let Ok(routes_table): LuaResult<Table> = lua.named_registry_value(ROUTES_KEY) else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for pair in routes_table.sequence_values::<Table>() {
            let Ok(entry) = pair else { continue };

            if let Some(route) = route_from_entry(&lua, &entry) {
                out.push(route);
            }
        }

        out
    }

    /// Execute a custom-route handler in **pool-mode** — identical model to
    /// [`run_job_handler`](Self::run_job_handler): no outer transaction, each Lua
    /// CRUD call autocommits, and `crap.transaction(fn)` opts into a shared tx.
    ///
    /// Builds the [`RouteContext`](crate::hooks::lifecycle::RouteContext) from
    /// `input`, calls `handler(ctx)`, and decodes the return value into a
    /// [`RouteResponse`].
    ///
    /// # Errors
    /// Returns an error if VM acquisition, handler resolution, the handler call,
    /// or response decoding fails.
    pub fn run_route_handler(
        &self,
        handler: &HookRef,
        input: &RouteHandlerInput,
        pool: &DbPool,
        infra: Option<LuaCrudInfra>,
    ) -> Result<RouteResponse> {
        // Thread a per-invocation event queue so route CRUD writes publish
        // live-update events and invalidate the populate cache like every
        // other pool-mode surface (jobs do this via `job_crud_infra`); the
        // queue flushes AFTER the handler, once the VM lease is released.
        let event_queue: Option<EventQueue> =
            infra.as_ref().map(|_| Rc::new(RefCell::new(Vec::new())));
        let event_transport = infra.as_ref().and_then(|i| i.event_transport.clone());
        let infra = infra.map(|mut i| {
            i.event_queue.clone_from(&event_queue);
            i
        });

        let response = {
            let lua = self.pool.acquire()?;
            let _guard = TxContextGuard::set_pool(
                &lua,
                pool.clone(),
                input.user.clone(),
                input.ui_locale.clone(),
                infra,
            );

            let ctx_value = lua.to_value(&input.context())?;
            let func = resolve_hook_function(&lua, handler.reference())?;
            let ret: Value = func.call(ctx_value)?;

            decode_route_response(ret)?
        };

        if let Some(queue) = event_queue {
            let flush_ctx = ServiceContext::slug_only("")
                .runner(self)
                .event_transport(event_transport)
                .build();
            flush_queue(&flush_ctx, &queue);
        }

        Ok(response)
    }

    /// Evaluate a custom route's `access` gate. The gate function receives the
    /// same [`RouteContext`](crate::hooks::lifecycle::RouteContext) as the
    /// handler and returns truthy to allow. Runs in pool-mode so the gate may
    /// read via `crap.collections.*`.
    ///
    /// # Errors
    /// Returns an error if VM acquisition, gate resolution, or the call fails.
    pub fn run_route_access(
        &self,
        access: &HookRef,
        input: &RouteHandlerInput,
        pool: &DbPool,
    ) -> Result<bool> {
        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set_pool(
            &lua,
            pool.clone(),
            input.user.clone(),
            input.ui_locale.clone(),
            None,
        );

        let ctx_value = lua.to_value(&input.context())?;
        let func = resolve_hook_function(&lua, access.reference())?;
        let ret: Value = func.call(ctx_value)?;

        Ok(!matches!(ret, Value::Nil | Value::Boolean(false)))
    }
}

/// Decode one registry entry table into a [`RouteDefinition`].
///
/// `crap.routes.register` validates and normalizes the shape at load time, so a
/// decode failure here means a corrupted entry — fail CLOSED (drop the route)
/// rather than mounting it without its access gate or with bad metadata.
fn route_from_entry(lua: &Lua, entry: &Table) -> Option<RouteDefinition> {
    let path: String = entry.get("path").ok()?;

    let methods_tbl: Table = entry.get("methods").ok()?;
    let mut methods = Vec::new();
    for m in methods_tbl.sequence_values::<String>() {
        methods.push(m.ok()?);
    }
    if methods.is_empty() {
        return None;
    }

    let handler = lua.from_value::<HookRef>(entry.get("handler").ok()?).ok()?;

    let access = match entry.get::<Value>("access") {
        Ok(Value::Nil | Value::Boolean(true)) | Err(_) => RouteAccess::Public,
        Ok(Value::Boolean(false)) => RouteAccess::Disabled,
        Ok(v) => match lua.from_value::<HookRef>(v) {
            Ok(hook_ref) => RouteAccess::Gated(hook_ref),
            Err(e) => {
                warn!(
                    "custom route '{path}': undecodable access value ({e}); \
                     dropping the route instead of serving it ungated"
                );
                return None;
            }
        },
    };

    let rate_limit = match entry.get::<Value>("rate_limit") {
        Ok(Value::Table(rl)) => {
            let max: u32 = rl.get("max").ok()?;
            let window: u64 = rl.get("window").ok()?;
            Some(RouteRateLimit::new(max, window))
        }
        _ => None,
    };

    let csrf = entry.get::<bool>("csrf").unwrap_or(false);

    let max_body = entry
        .get::<Option<i64>>("max_body")
        .ok()
        .flatten()
        .and_then(|n| u64::try_from(n).ok());

    let options = match entry.get::<Value>("options") {
        Ok(v @ Value::Table(_)) => lua_api::lua_to_json(&v).ok(),
        _ => None,
    };

    Some(RouteDefinition {
        path,
        methods,
        handler,
        access,
        rate_limit,
        csrf,
        max_body,
        options,
    })
}

/// Decode a route handler's return value into a [`RouteResponse`].
///
/// - `nil` / `false` → 404.
/// - a bare string → 200 with that string as the body.
/// - a table → the response envelope (`status`, `headers`, `cookies`,
///   `redirect`, `body` / `json`).
pub(crate) fn decode_route_response(value: Value) -> Result<RouteResponse> {
    match value {
        Value::Nil | Value::Boolean(false) => Ok(RouteResponse::not_found()),
        Value::String(s) => Ok(RouteResponse {
            body: RouteBody::Text(s.to_str()?.to_string()),
            ..RouteResponse::default()
        }),
        Value::Table(t) => decode_envelope(&t),
        other => Err(anyhow!(
            "route handler must return a table, a string, or nil — got {}",
            other.type_name()
        )),
    }
}

fn decode_envelope(t: &Table) -> Result<RouteResponse> {
    deny_unknown_keys(
        t,
        "route response",
        &["status", "headers", "cookies", "redirect", "body", "json"],
    )?;

    let redirect: Option<String> = t.get("redirect")?;
    let explicit_status: Option<u16> = t.get("status")?;

    // A `redirect` with a non-3xx explicit status is incoherent (e.g. a 200 with
    // a `Location` is silently ignored by browsers) — reject it loudly.
    if redirect.is_some() && explicit_status.is_some_and(|s| !(300..=399).contains(&s)) {
        return Err(anyhow!(
            "route response `redirect` requires a 3xx `status` (got {})",
            explicit_status.unwrap_or(0)
        ));
    }

    let status = explicit_status.unwrap_or(if redirect.is_some() { 302 } else { 200 });

    let mut headers = Vec::new();
    if let Value::Table(h) = t.get::<Value>("headers")? {
        for pair in h.pairs::<String, String>() {
            let (k, v) = pair?;
            headers.push((k, v));
        }
    }

    let mut cookies = Vec::new();
    if let Value::Table(cs) = t.get::<Value>("cookies")? {
        for entry in cs.sequence_values::<Table>() {
            cookies.push(decode_cookie(&entry?)?);
        }
    }

    // `json` wins over `body` when both are present.
    let body = match t.get::<Value>("json")? {
        Value::Nil => match t.get::<Value>("body")? {
            Value::Nil => RouteBody::Empty,
            Value::String(s) => RouteBody::Text(s.to_str()?.to_string()),
            other => {
                return Err(anyhow!(
                    "route response `body` must be a string (use `json` for structured data), got {}",
                    other.type_name()
                ));
            }
        },
        json => RouteBody::Json(lua_api::lua_to_json(&json)?),
    };

    Ok(RouteResponse {
        status,
        headers,
        cookies,
        redirect,
        body,
    })
}

fn decode_cookie(t: &Table) -> Result<RouteCookie> {
    deny_unknown_keys(
        t,
        "route response cookie",
        &[
            "name",
            "value",
            "http_only",
            "secure",
            "same_site",
            "path",
            "max_age",
            "domain",
        ],
    )?;

    let name: String = t
        .get("name")
        .map_err(|_| anyhow!("response cookie requires a string `name`"))?;
    let value: String = t
        .get("value")
        .map_err(|_| anyhow!("response cookie requires a string `value`"))?;

    // Reject characters that would let a handler inject extra `Set-Cookie`
    // attributes (`;`, space) or break the header — e.g. a value derived from
    // request input must not be able to smuggle `; Domain=…`.
    if !is_valid_cookie_name(&name) {
        return Err(anyhow!(
            "response cookie name {name:?} contains invalid characters"
        ));
    }
    if !is_valid_cookie_value(&value) {
        return Err(anyhow!(
            "response cookie value for {name:?} contains invalid characters \
             (no spaces, control chars, or any of \" , ; \\)"
        ));
    }

    // `path`/`domain` land verbatim in `Set-Cookie`, so they must not be able to
    // smuggle extra `; Attr=…` segments either; `same_site` must be a known value.
    let path: Option<String> = t.get("path")?;
    if let Some(p) = path.as_deref().filter(|p| !is_valid_cookie_av(p)) {
        return Err(anyhow!(
            "response cookie `path` {p:?} contains invalid characters"
        ));
    }
    let domain: Option<String> = t.get("domain")?;
    if let Some(d) = domain.as_deref().filter(|d| !is_valid_cookie_av(d)) {
        return Err(anyhow!(
            "response cookie `domain` {d:?} contains invalid characters"
        ));
    }
    let same_site: Option<String> = t.get("same_site")?;
    if let Some(ss) = same_site
        .as_deref()
        .filter(|ss| !matches!(ss.to_ascii_lowercase().as_str(), "lax" | "strict" | "none"))
    {
        return Err(anyhow!(
            "response cookie `same_site` must be \"lax\", \"strict\", or \"none\" (got {ss:?})"
        ));
    }

    Ok(RouteCookie {
        name,
        value,
        http_only: t.get::<Option<bool>>("http_only")?.unwrap_or(false),
        secure: t.get::<Option<bool>>("secure")?.unwrap_or(false),
        same_site,
        path,
        max_age: t.get::<Option<i64>>("max_age")?,
        domain,
    })
}

/// RFC 6265 cookie-name (an RFC 2616 token): non-empty, no control chars,
/// whitespace, or separators.
fn is_valid_cookie_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_graphic()
                && !matches!(
                    b,
                    b'(' | b')'
                        | b'<'
                        | b'>'
                        | b'@'
                        | b','
                        | b';'
                        | b':'
                        | b'\\'
                        | b'"'
                        | b'/'
                        | b'['
                        | b']'
                        | b'?'
                        | b'='
                        | b'{'
                        | b'}'
                )
        })
}

/// RFC 6265 cookie-value (`cookie-octet`): printable ASCII excluding space,
/// DQUOTE, comma, semicolon, and backslash. Empty is allowed (cookie deletion).
fn is_valid_cookie_value(s: &str) -> bool {
    s.bytes()
        .all(|b| matches!(b, 0x21 | 0x23..=0x2B | 0x2D..=0x3A | 0x3C..=0x5B | 0x5D..=0x7E))
}

/// A cookie attribute value (`Path`/`Domain`): printable ASCII with no control
/// chars and, crucially, no `;` — so it can't smuggle additional attributes.
fn is_valid_cookie_av(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7E).contains(&b) && b != b';')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(code: &str) -> Result<RouteResponse> {
        let lua = Lua::new();
        let v: Value = lua.load(code).eval().unwrap();
        decode_route_response(v)
    }

    #[test]
    fn nil_is_404() {
        let r = decode("return nil").unwrap();
        assert_eq!(r.status, 404);
        assert!(matches!(r.body, RouteBody::Empty));
    }

    #[test]
    fn bare_string_is_200_text() {
        let r = decode(r#"return "hello""#).unwrap();
        assert_eq!(r.status, 200);
        assert!(matches!(r.body, RouteBody::Text(s) if s == "hello"));
    }

    #[test]
    fn json_envelope() {
        let r = decode(r"return { json = { ok = true }, status = 201 }").unwrap();
        assert_eq!(r.status, 201);
        assert!(matches!(r.body, RouteBody::Json(_)));
    }

    #[test]
    fn redirect_defaults_to_302() {
        let r = decode(r#"return { redirect = "/somewhere" }"#).unwrap();
        assert_eq!(r.status, 302);
        assert_eq!(r.redirect.as_deref(), Some("/somewhere"));
    }

    #[test]
    fn cookies_and_headers_decode() {
        let r = decode(
            r#"return {
                headers = { ["x-foo"] = "bar" },
                cookies = { { name = "s", value = "v", http_only = true, same_site = "lax", max_age = 600 } },
            }"#,
        )
        .unwrap();
        assert_eq!(r.headers, vec![("x-foo".to_string(), "bar".to_string())]);
        assert_eq!(r.cookies.len(), 1);
        let c = &r.cookies[0];
        assert_eq!(c.name, "s");
        assert!(c.http_only);
        assert_eq!(c.same_site.as_deref(), Some("lax"));
        assert_eq!(c.max_age, Some(600));
    }

    #[test]
    fn unknown_envelope_key_rejected() {
        let err = decode(r"return { staus = 200 }").unwrap_err().to_string();
        assert!(err.contains("staus"), "{err}");
    }

    #[test]
    fn cookie_requires_name_and_value() {
        let err = decode(r#"return { cookies = { { value = "v" } } }"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("name"), "{err}");
    }

    #[test]
    fn non_envelope_return_errors() {
        let err = decode("return 42").unwrap_err().to_string();
        assert!(err.contains("must return"), "{err}");
    }

    #[test]
    fn cookie_value_injection_rejected() {
        let err =
            decode(r#"return { cookies = { { name = "sid", value = "x; Domain=evil.com" } } }"#)
                .unwrap_err()
                .to_string();
        assert!(err.contains("invalid characters"), "{err}");

        let bad_name = decode(r#"return { cookies = { { name = "a b", value = "v" } } }"#)
            .unwrap_err()
            .to_string();
        assert!(bad_name.contains("invalid characters"), "{bad_name}");

        // path / domain can't smuggle extra attributes
        let bad_path = decode(
            r#"return { cookies = { { name = "s", value = "v", path = "/; Domain=evil.com" } } }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(bad_path.contains("path"), "{bad_path}");

        let bad_domain =
            decode(r#"return { cookies = { { name = "s", value = "v", domain = "x; Secure" } } }"#)
                .unwrap_err()
                .to_string();
        assert!(bad_domain.contains("domain"), "{bad_domain}");

        // same_site must be a known value
        let bad_ss = decode(
            r#"return { cookies = { { name = "s", value = "v", same_site = "Lax; HttpOnly" } } }"#,
        )
        .unwrap_err()
        .to_string();
        assert!(bad_ss.contains("same_site"), "{bad_ss}");
    }

    #[test]
    fn redirect_with_non_3xx_status_rejected() {
        let err = decode(r#"return { redirect = "/x", status = 200 }"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("3xx"), "{err}");
        // a 3xx explicit status is fine
        assert!(decode(r#"return { redirect = "/x", status = 301 }"#).is_ok());
    }
}
