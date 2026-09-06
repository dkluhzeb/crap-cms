//! Generic dispatcher for custom routes declared via `crap.routes.register`.
//!
//! One instance is mounted per registered route (with the route's
//! [`MountedRoute`] injected as a request extension). It applies the per-route
//! gates (rate limit, optional CSRF), resolves the principal, evaluates the
//! optional access gate, runs the Lua handler in pool-mode off the async
//! runtime, and converts the returned envelope into an Axum response.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, Extension, RawPathParams, Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE},
    },
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq as _;
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        custom_routes::{RouteAccess, RouteBody, RouteCookie, RouteDefinition, RouteResponse},
        handlers::{
            auth::{CSRF_COOKIE, SESSION_COOKIE, client_ip, headers_to_map},
            custom_route::MountedRoute,
        },
        server::extract_cookie,
    },
    core::Document,
    core::collection::Surface,
    hooks::lifecycle::{LuaCrudInfra, RouteHandlerInput},
    service::auth::{AuthRequest, EvaluateDeps, Resolution, evaluate},
};

/// Request parts (owned) gathered from the extractors, grouped so the
/// input-builder stays within the argument budget.
struct RawRequest {
    method: Method,
    params: HashMap<String, String>,
    query: HashMap<String, String>,
    headers: HeaderMap,
    body: Bytes,
    ip: String,
}

/// The authenticated principal resolved from the request (none when anonymous —
/// custom routes are public, so resolution never rejects).
struct Principal {
    user: Document,
    collection: String,
    ui_locale: Option<String>,
}

/// Everything the blocking phase needs, grouped to satisfy the param-count rule.
struct DispatchJob {
    state: AdminState,
    mounted: Arc<MountedRoute>,
    raw: RawRequest,
}

/// Outcome of the blocking phase: an access denial or the handler's response.
enum DispatchOutcome {
    Forbidden,
    Response(RouteResponse),
}

/// Parse all request cookies into a `name → value` map.
fn parse_cookies(headers: &HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for value in headers.get_all(COOKIE) {
        let Ok(raw) = value.to_str() else { continue };
        for pair in raw.split(';') {
            if let Some((k, v)) = pair.split_once('=') {
                out.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    out
}

/// Parse a raw query string into a `key → value` map.
fn parse_query(raw: &str) -> HashMap<String, String> {
    form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .collect()
}

/// Parse the body by `Content-Type` into `(raw, json, form)`.
fn parse_body(
    headers: &HeaderMap,
    body: &Bytes,
) -> (
    Option<String>,
    Option<serde_json::Value>,
    Option<HashMap<String, String>>,
) {
    if body.is_empty() {
        return (None, None, None);
    }

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let raw = Some(String::from_utf8_lossy(body).into_owned());

    if content_type.contains("application/json") {
        let json = serde_json::from_slice::<serde_json::Value>(body).ok();
        return (raw, json, None);
    }

    if content_type.contains("application/x-www-form-urlencoded") {
        let form = form_urlencoded::parse(body).into_owned().collect();
        return (raw, None, Some(form));
    }

    (raw, None, None)
}

/// Whether the method mutates state (and so requires a CSRF token when the route
/// opts into CSRF protection).
fn is_mutating(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    )
}

/// Double-submit CSRF check: the `crap_csrf` cookie must match the
/// `X-CSRF-Token` header (constant-time). Only consulted for routes with
/// `csrf = true`.
fn csrf_valid(headers: &HeaderMap) -> bool {
    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(cookie) = extract_cookie(cookie_header, CSRF_COOKIE).filter(|c| !c.is_empty()) else {
        return false;
    };
    let Some(header) = headers.get("x-csrf-token").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    cookie.as_bytes().ct_eq(header.as_bytes()).into()
}

/// Resolve the request's principal via the unified auth evaluator. Returns
/// `None` for anonymous / invalid / lookup-error — custom routes are public, so
/// a failed resolution is simply an anonymous request, never a rejection.
///
/// `allow_cookie` gates the **session cookie** credential: it is honored only
/// for safe methods or when the route opts into CSRF (`csrf = true`). A mutating
/// route without CSRF therefore can't act on the cookie identity (which a
/// browser would auto-attach cross-site) — only a `Bearer` token, which is not
/// CSRF-able, authenticates such a request. Mirrors the global CSRF middleware.
fn resolve_principal(
    state: &AdminState,
    headers: &HeaderMap,
    allow_cookie: bool,
) -> Option<Principal> {
    let conn = state.infra.pool.get().ok()?;

    let cookie_header = headers
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let session = if allow_cookie {
        extract_cookie(cookie_header, SESSION_COOKIE).map(str::to_string)
    } else {
        None
    };
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let headers_map = headers_to_map(headers);

    let resolution = evaluate(
        &AuthRequest {
            surface: Surface::Admin,
            bearer_token: bearer.as_deref(),
            session_cookie_token: session.as_deref(),
            headers: &headers_map,
        },
        &EvaluateDeps {
            registry: &state.infra.registry,
            token_provider: state.infra.token_provider.as_ref(),
            hook_runner: &state.infra.hook_runner,
            conn: &conn,
        },
    );

    match resolution {
        Resolution::Authenticated(auth) => Some(Principal {
            collection: auth.user.claims.collection.to_string(),
            ui_locale: Some(auth.user.ui_locale),
            user: auth.user.user_doc,
        }),
        _ => None,
    }
}

/// Assemble the owned [`RouteHandlerInput`] from the request, route metadata, and
/// resolved principal.
fn build_input(
    def: &RouteDefinition,
    raw: RawRequest,
    principal: Option<Principal>,
) -> RouteHandlerInput {
    let (body, json, form) = parse_body(&raw.headers, &raw.body);
    let cookies = parse_cookies(&raw.headers);
    let (user, collection, ui_locale) = match principal {
        Some(p) => (Some(p.user), Some(p.collection), p.ui_locale),
        None => (None, None, None),
    };

    RouteHandlerInput {
        method: raw.method.as_str().to_string(),
        path: def.path.clone(),
        params: raw.params,
        query: raw.query,
        headers: headers_to_map(&raw.headers),
        cookies,
        body,
        json,
        form,
        user,
        collection,
        ip: raw.ip,
        ui_locale,
        options: def.options.clone(),
    }
}

/// The blocking phase: resolve the principal, evaluate the access gate, and run
/// the handler. Extracted from the handler per the spawn-blocking rule.
fn run_dispatch_blocking(job: DispatchJob) -> Result<DispatchOutcome> {
    let DispatchJob {
        state,
        mounted,
        raw,
    } = job;
    let def = &mounted.def;

    // The session cookie authenticates a mutating request only when the route
    // opts into CSRF; otherwise (and always for Bearer) it would be a CSRF hole.
    let allow_cookie = !is_mutating(&raw.method) || def.csrf;
    let principal = resolve_principal(&state, &raw.headers, allow_cookie);
    let input = build_input(def, raw, principal);

    let allowed = match &def.access {
        RouteAccess::Gated(access) => {
            state
                .infra
                .hook_runner
                .run_route_access(access, &input, &state.infra.pool)?
        }
        // Disabled is rejected before the blocking phase; Public allows.
        _ => true,
    };
    if !allowed {
        return Ok(DispatchOutcome::Forbidden);
    }

    // Route CRUD writes must invalidate the populate cache and publish
    // live events like every other pool-mode surface
    // — thread the event transport + cache from AppInfra, mirroring the
    // scheduler's `job_crud_infra`.
    let route_infra = LuaCrudInfra::for_pool_crud(&state.infra);
    let resp = state.infra.hook_runner.run_route_handler(
        &def.handler,
        &input,
        &state.infra.pool,
        Some(route_infra),
    )?;
    Ok(DispatchOutcome::Response(resp))
}

/// Render the `name=value; ...` `Set-Cookie` value for a route cookie.
fn route_cookie_header(c: &RouteCookie) -> String {
    let mut s = format!("{}={}", c.name, c.value);
    if let Some(path) = &c.path {
        s.push_str("; Path=");
        s.push_str(path);
    }
    if let Some(domain) = &c.domain {
        s.push_str("; Domain=");
        s.push_str(domain);
    }
    if let Some(max_age) = c.max_age {
        s.push_str("; Max-Age=");
        s.push_str(&max_age.to_string());
    }
    let is_none_same_site = c
        .same_site
        .as_deref()
        .is_some_and(|ss| ss.eq_ignore_ascii_case("none"));
    if let Some(same_site) = &c.same_site {
        let canonical = match same_site.to_ascii_lowercase().as_str() {
            "lax" => "Lax",
            "strict" => "Strict",
            "none" => "None",
            _ => same_site.as_str(),
        };
        s.push_str("; SameSite=");
        s.push_str(canonical);
    }
    if c.http_only {
        s.push_str("; HttpOnly");
    }
    // `SameSite=None` is rejected by browsers without `Secure`, so force it.
    if c.secure || is_none_same_site {
        s.push_str("; Secure");
    }
    s
}

/// Convert a decoded [`RouteResponse`] into an Axum [`Response`].
fn response_to_axum(resp: RouteResponse) -> Response {
    let status = StatusCode::from_u16(resp.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let (body, default_ct): (Body, Option<&'static str>) = match resp.body {
        RouteBody::Empty => (Body::empty(), None),
        RouteBody::Text(s) => (Body::from(s), Some("text/plain; charset=utf-8")),
        RouteBody::Json(v) => match serde_json::to_vec(&v) {
            Ok(bytes) => (Body::from(bytes), Some("application/json")),
            Err(e) => {
                error!("custom route: failed to serialize JSON response: {e}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        },
    };

    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();

    if let Some(ct) = default_ct {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(ct));
    }

    for (name, value) in resp.headers {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            headers.insert(n, v);
        } else {
            warn!("custom route: dropping invalid response header {name:?}");
        }
    }

    if let Some(location) = resp.redirect {
        if let Ok(v) = HeaderValue::from_str(&location) {
            headers.insert(LOCATION, v);
        } else {
            warn!("custom route: invalid redirect location {location:?}");
        }
    }

    for cookie in &resp.cookies {
        if let Ok(v) = HeaderValue::from_str(&route_cookie_header(cookie)) {
            headers.append(SET_COOKIE, v);
        } else {
            warn!("custom route: invalid cookie {:?}", cookie.name);
        }
    }

    response
}

/// Generic handler for a custom route. The route's [`MountedRoute`] arrives as a
/// request extension (injected per-route at mount time).
pub async fn dispatch_custom_route(
    Extension(mounted): Extension<Arc<MountedRoute>>,
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    raw_params: RawPathParams,
    request: Request,
) -> Response {
    let def = &mounted.def;

    // A disabled route (`access = false`) is registered but not served.
    if matches!(def.access, RouteAccess::Disabled) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let method = request.method().clone();
    let headers = request.headers().clone();
    let ip = client_ip(&headers, &addr, &state.config.server);

    // Per-route rate limit (recorded atomically up front).
    if mounted
        .limiter
        .as_ref()
        .is_some_and(|limiter| limiter.check_and_block(&ip))
    {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    // Optional CSRF for cookie-authenticated mutating routes.
    if def.csrf && is_mutating(&method) && !csrf_valid(&headers) {
        return (StatusCode::FORBIDDEN, "CSRF validation failed").into_response();
    }

    let params = raw_params
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let query = request.uri().query().map(parse_query).unwrap_or_default();

    // The `DefaultBodyLimit` layer already caps the body; passing the route's
    // own limit here is defense-in-depth (so it still holds if the layer is ever
    // refactored away) rather than relying on `usize::MAX`.
    let Ok(body) = to_bytes(request.into_body(), mounted.body_limit).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };

    let raw = RawRequest {
        method,
        params,
        query,
        headers,
        body,
        ip,
    };
    let job = DispatchJob {
        state,
        mounted,
        raw,
    };

    match task::spawn_blocking(move || run_dispatch_blocking(job)).await {
        Ok(Ok(DispatchOutcome::Response(resp))) => response_to_axum(resp),
        Ok(Ok(DispatchOutcome::Forbidden)) => StatusCode::FORBIDDEN.into_response(),
        Ok(Err(e)) => {
            error!("custom route handler error: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(e) => {
            error!("custom route task error: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_header_has_attributes() {
        let c = RouteCookie {
            name: "s".into(),
            value: "v".into(),
            http_only: true,
            secure: true,
            same_site: Some("lax".into()),
            path: Some("/auth".into()),
            max_age: Some(600),
            domain: None,
        };
        let h = route_cookie_header(&c);
        assert!(h.starts_with("s=v"));
        assert!(h.contains("; Path=/auth"));
        assert!(h.contains("; Max-Age=600"));
        assert!(h.contains("; SameSite=Lax"));
        assert!(h.contains("; HttpOnly"));
        assert!(h.contains("; Secure"));
    }

    #[test]
    fn parse_cookies_splits_pairs() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, "a=1; b=2; crap_session=tok".parse().unwrap());
        let cookies = parse_cookies(&headers);
        assert_eq!(cookies.get("a").map(String::as_str), Some("1"));
        assert_eq!(cookies.get("crap_session").map(String::as_str), Some("tok"));
    }

    #[test]
    fn parse_body_detects_json_and_form() {
        let mut json_headers = HeaderMap::new();
        json_headers.insert(CONTENT_TYPE, "application/json".parse().unwrap());
        let (_, json, _) = parse_body(&json_headers, &Bytes::from_static(b"{\"a\":1}"));
        assert!(json.is_some());

        let mut form_headers = HeaderMap::new();
        form_headers.insert(
            CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        let (_, _, form) = parse_body(&form_headers, &Bytes::from_static(b"a=1&b=2"));
        assert_eq!(form.unwrap().get("b").map(String::as_str), Some("2"));
    }

    #[test]
    fn csrf_requires_matching_cookie_and_header() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, "crap_csrf=tok123".parse().unwrap());
        headers.insert("x-csrf-token", "tok123".parse().unwrap());
        assert!(csrf_valid(&headers));

        let mut mismatch = HeaderMap::new();
        mismatch.insert(COOKIE, "crap_csrf=tok123".parse().unwrap());
        mismatch.insert("x-csrf-token", "different".parse().unwrap());
        assert!(!csrf_valid(&mismatch));

        // Missing header
        let mut no_header = HeaderMap::new();
        no_header.insert(COOKIE, "crap_csrf=tok123".parse().unwrap());
        assert!(!csrf_valid(&no_header));
    }
}
