//! Custom HTTP route registry — populated from Lua via [`crap.routes.register`].
//!
//! A custom route mounts an arbitrary HTTP endpoint on the admin server,
//! dispatched to a Lua handler ref (`routes/<name>.lua`). Unlike custom *pages*
//! (which render an admin-gated HTML template), custom *routes* are public by
//! default and return a structured response envelope (status / headers / cookies
//! / body / redirect). The handler runs in pool-mode — identical to a job
//! handler — so per-op CRUD autocommits and `crap.transaction(fn)` provides
//! atomic blocks.
//!
//! Customer flow:
//!
//! 1. Drop a handler at `<config_dir>/routes/<name>.lua` returning
//!    `function(ctx) ... return { ... } end`.
//! 2. Register it in `init.lua`:
//!    `crap.routes.register({ path = "/webhooks/x", method = "POST",
//!    handler = "routes.x" })`.
//!
//! The registry is read once at startup (mirrors [`crate::admin::custom_pages`]).

use crate::core::HookRef;

/// HTTP methods a custom route may register for. Anything outside this set is
/// rejected at registration time.
pub const ALLOWED_METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Path prefixes owned by the built-in router. A custom route may not register
/// a path under any of these — checked at registration so a typo can't shadow
/// (or be shadowed by) a core route (and so router assembly never panics on an
/// overlap). `/` is reserved exactly (the dashboard root).
pub const RESERVED_PREFIXES: &[&str] = &[
    "/", "/admin", "/static", "/api", "/uploads", "/mcp", "/health", "/ready",
];

/// Access stance for a custom route.
#[derive(Clone, Debug)]
pub enum RouteAccess {
    /// Reachable by anyone (the default when `access` is omitted, or `true`).
    Public,
    /// Registered but not served — responds 404 (`access = false`).
    Disabled,
    /// Gated by a Lua access function evaluated with the route context before
    /// the handler runs; a falsey return is a 403.
    Gated(HookRef),
}

/// Optional per-route rate limit (`{ max, window }`, window in seconds).
#[derive(Clone, Copy, Debug)]
pub struct RouteRateLimit {
    pub max: u32,
    pub window: u64,
}

impl RouteRateLimit {
    #[must_use]
    pub fn new(max: u32, window: u64) -> Self {
        Self { max, window }
    }
}

/// A custom route declared from Lua via `crap.routes.register`.
#[derive(Clone, Debug)]
pub struct RouteDefinition {
    /// The path the route mounts at (after the optional configured prefix).
    /// Uses Axum `{param}` syntax for path params.
    pub path: String,
    /// HTTP methods this route answers, upper-cased and validated.
    pub methods: Vec<String>,
    /// The Lua handler ref (`routes.<name>` → `routes/<name>.lua`).
    pub handler: HookRef,
    /// Access stance — see [`RouteAccess`].
    pub access: RouteAccess,
    /// Optional per-route rate limit.
    pub rate_limit: Option<RouteRateLimit>,
    /// Whether to enforce the admin CSRF token (off by default — custom routes
    /// are API-style; opt in for a cookie-authenticated mutating route).
    pub csrf: bool,
    /// Per-route request body-size limit in bytes; `None` falls back to the
    /// `routes.max_body` config default.
    pub max_body: Option<u64>,
    /// Arbitrary per-route options, surfaced to the handler as `ctx.options`.
    pub options: Option<serde_json::Value>,
}

impl RouteDefinition {
    /// A stable identifier for logging / rate-limit keying: `METHOD,METHOD path`.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{} {}", self.methods.join(","), self.path)
    }
}

/// A `Set-Cookie` the handler asked to emit (decoded from the response
/// envelope's `cookies` array).
#[derive(Clone, Debug)]
pub struct RouteCookie {
    pub name: String,
    pub value: String,
    pub http_only: bool,
    pub secure: bool,
    /// `"lax"` / `"strict"` / `"none"`, or `None` to omit the attribute.
    pub same_site: Option<String>,
    pub path: Option<String>,
    pub max_age: Option<i64>,
    pub domain: Option<String>,
}

/// The body of a route response.
#[derive(Clone, Debug, Default)]
pub enum RouteBody {
    /// No body (e.g. a redirect or a bare status).
    #[default]
    Empty,
    /// A raw string body (defaults to `text/plain` unless a `Content-Type`
    /// header is set).
    Text(String),
    /// A JSON body — serialized and sent with `Content-Type: application/json`.
    Json(serde_json::Value),
}

/// The decoded response envelope a route handler returns. Converted to an Axum
/// `Response` by the route dispatcher.
#[derive(Clone, Debug)]
pub struct RouteResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<RouteCookie>,
    /// When set, the response is a redirect to this location (with `status` as
    /// the redirect code, default 302).
    pub redirect: Option<String>,
    pub body: RouteBody,
}

impl Default for RouteResponse {
    fn default() -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            cookies: Vec::new(),
            redirect: None,
            body: RouteBody::Empty,
        }
    }
}

impl RouteResponse {
    /// The response for a handler that returned `nil`/`false`.
    #[must_use]
    pub fn not_found() -> Self {
        Self {
            status: 404,
            ..Self::default()
        }
    }
}

/// Validate an HTTP method name (case-insensitive); returns the upper-cased
/// form on success.
///
/// # Errors
/// Returns the offending method when it is not in [`ALLOWED_METHODS`].
pub fn normalize_method(method: &str) -> Result<String, String> {
    let upper = method.trim().to_ascii_uppercase();
    if ALLOWED_METHODS.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        Err(upper)
    }
}

/// Validate a custom-route path. Must be absolute, free of traversal, and not
/// under a [`RESERVED_PREFIXES`] segment.
///
/// # Errors
/// Returns a human-readable reason when the path is invalid.
pub fn validate_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err(format!("path {path:?} must start with '/'"));
    }
    if path.contains("..") {
        return Err(format!("path {path:?} must not contain '..'"));
    }
    if path.contains("//") {
        return Err(format!("path {path:?} must not contain '//'"));
    }
    if path.len() > 1 && path.ends_with('/') {
        return Err(format!("path {path:?} must not end with '/'"));
    }
    if is_reserved_path(path) {
        return Err(format!(
            "path {path:?} collides with a reserved prefix ({})",
            RESERVED_PREFIXES.join(", ")
        ));
    }
    Ok(())
}

/// Whether `path` falls under a reserved built-in prefix (exact segment match,
/// so `/administer` is allowed but `/admin` and `/admin/x` are not).
#[must_use]
pub fn is_reserved_path(path: &str) -> bool {
    RESERVED_PREFIXES
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_method_accepts_known_lowercase() {
        assert_eq!(normalize_method("post").unwrap(), "POST");
        assert_eq!(normalize_method(" Get ").unwrap(), "GET");
    }

    #[test]
    fn normalize_method_rejects_unknown() {
        assert_eq!(normalize_method("TRACE").unwrap_err(), "TRACE");
        assert_eq!(normalize_method("frobnicate").unwrap_err(), "FROBNICATE");
    }

    #[test]
    fn validate_path_accepts_good_paths() {
        assert!(validate_path("/webhooks/stripe").is_ok());
        assert!(validate_path("/auth/start/google").is_ok());
        assert!(validate_path("/items/{id}").is_ok());
        assert!(
            validate_path("/administer").is_ok(),
            "not a reserved segment"
        );
    }

    #[test]
    fn validate_path_rejects_bad_paths() {
        assert!(validate_path("webhooks").is_err(), "must be absolute");
        assert!(validate_path("/../etc").is_err(), "traversal");
        assert!(validate_path("/a//b").is_err(), "double slash");
        assert!(validate_path("/trailing/").is_err(), "trailing slash");
    }

    #[test]
    fn reserved_prefixes_are_blocked() {
        assert!(is_reserved_path("/admin"));
        assert!(is_reserved_path("/admin/collections/posts"));
        assert!(is_reserved_path("/static"));
        assert!(is_reserved_path("/static/app.css"));
        assert!(is_reserved_path("/api/upload"));
        assert!(is_reserved_path("/uploads/x/y"));
        assert!(is_reserved_path("/"));
        assert!(!is_reserved_path("/administer"));
        assert!(!is_reserved_path("/staticky"));
        assert!(!is_reserved_path("/apidocs"));
        assert!(!is_reserved_path("/webhooks/x"));
        assert!(validate_path("/admin/x").is_err());
        assert!(validate_path("/api/x").is_err());
    }

    #[test]
    fn route_id_is_stable() {
        let def = RouteDefinition {
            path: "/x".into(),
            methods: vec!["GET".into(), "POST".into()],
            handler: HookRef::new("routes.x"),
            access: RouteAccess::Public,
            rate_limit: None,
            csrf: false,
            max_body: None,
            options: None,
        };
        assert_eq!(def.id(), "GET,POST /x");
    }
}
