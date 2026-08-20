//! Unified per-request auth evaluator.
//!
//! Replaces the legacy per-handler dispatch (admin cookie path,
//! gRPC `extract_token` → `resolve_auth_user`, plus the separate
//! strategy fallback paths) with a single function that:
//!
//! 1. Looks at the bearer token (if any) and resolves the issuing
//!    collection — but only when that collection's `methods` list
//!    declares a [`AuthMethod::Bearer`] whose `surfaces` include
//!    the current request's surface.
//! 2. Same for the session-cookie JWT.
//! 3. Walks every auth collection's `methods` list in declaration
//!    order and, for each [`AuthMethod::Strategy`], checks that
//!    its `surfaces` covers the request surface AND its
//!    `activates_on` discriminator matches (either `Always` or
//!    `Header { name }` with the named header present). Invokes
//!    the Lua hook; first match that produces a document wins.
//!
//! `PasswordLogin` is NOT evaluated here — the Login RPC / form
//! POST handler invokes it explicitly because it consumes
//! credentials from the request body, not from request metadata.
//!
//! All three branches share the same locked-user + stale-session-
//! version rejection paths.

use std::collections::HashMap;

use chrono::Utc;
use tracing::{debug, warn};

use crate::core::{
    AuthUser, Claims, Document, Registry, Slug,
    auth::{ClaimsBuilder, TokenProvider},
    collection::{Auth, Surface},
    parse_truthy,
};
use crate::db::{DbConnection, query};
use crate::hooks::{HookRunner, lifecycle::AuthStrategyInput};
use crate::service::{self, ServiceContext};

/// Per-request inputs for [`evaluate`].
///
/// The evaluator never reads from `Request` directly — callers
/// (admin middleware, gRPC service) extract bearer/cookie/headers
/// once and hand them over. Keeps the evaluator pure + cheap to
/// unit-test.
pub struct AuthRequest<'a> {
    /// Which host transport is serving this request. Filters out
    /// any method whose `surfaces` doesn't include this value.
    pub surface: Surface,
    /// Value of the `Authorization: Bearer …` header / gRPC
    /// metadata, with the `Bearer ` prefix already stripped.
    /// `None` when no such header is present.
    pub bearer_token: Option<&'a str>,
    /// JWT value extracted from the `crap_session` cookie.
    /// Always `None` for gRPC.
    pub session_cookie_token: Option<&'a str>,
    /// All request headers, preserving the casing the transport
    /// delivered. Activation matching is case-insensitive on the
    /// evaluator side; preserving casing keeps the contract for
    /// Lua hooks that look up specific cases stable.
    pub headers: &'a HashMap<String, String>,
}

/// Borrowed bundle of every dependency the evaluator needs that
/// isn't request-specific. Constructed once per spawn-blocking call
/// in admin middleware / gRPC handlers; lets the evaluator stay a
/// single 2-arg call site per the >4-arg rule in `CLAUDE.md`.
pub struct EvaluateDeps<'a> {
    pub registry: &'a Registry,
    pub token_provider: &'a dyn TokenProvider,
    pub hook_runner: &'a HookRunner,
    pub conn: &'a dyn DbConnection,
}

/// Outcome of evaluating a request against the registry's auth
/// configuration.
///
/// The `Authenticated` variant boxes its payload — `AuthUser` carries
/// the full user document, which dwarfs the empty `Anonymous` and the
/// short `Invalid` discriminant. Boxing keeps every `Resolution` value
/// cheap to move along the success-and-failure-share-a-shape paths in
/// the middleware and gRPC handlers.
#[derive(Debug)]
pub enum Resolution {
    /// A method matched and produced a principal.
    Authenticated(Box<AuthenticatedResolution>),
    /// No credentials present, or every credential's issuing
    /// collection doesn't accept that credential on this surface,
    /// and no strategy matched. Caller decides whether to return
    /// 401, redirect to login, or proceed anonymously.
    Anonymous,
    /// A credential was supplied but is unusable. Caller picks the
    /// response (clear the stale cookie, return 401 with a precise
    /// reason, log the locked account, …) based on the specific
    /// [`AuthFailure`] — `Invalid` is never silently equivalent to
    /// `Anonymous`, otherwise a holder of an invalidated token gets
    /// the same treatment as a true anonymous caller.
    Invalid(AuthFailure),
}

/// Why a supplied credential was rejected. Carried by
/// [`Resolution::Invalid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailure {
    /// Token signature invalid, malformed, or expired (JWT-level).
    BadToken,
    /// The auth collection named in the token's claims no longer
    /// exists in the registry — config drift since issuance.
    UnknownCollection,
    /// The user document referenced by the token's `sub` is missing
    /// (deleted since issuance).
    UserMissing,
    /// Transient DB error while looking up the user or session state.
    Lookup,
    /// User exists but is `_locked = 1`.
    Locked,
    /// Token's `session_version` doesn't match the user's current
    /// version (password changed, forced logout).
    StaleSession,
    /// A token decoded fine but its issuing collection no longer
    /// accepts that credential type on the current surface (and no
    /// other method authenticated). Distinct from `BadToken` because
    /// the token's signature was valid — the *issuer* has changed,
    /// not the token. Distinct from a silent `Anonymous` so admin
    /// clears the now-dead cookie instead of redirect-looping.
    Unaccepted,
}

impl AuthFailure {
    /// Short, operator-friendly label for log messages / debug
    /// headers. Stable string — safe to switch on.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadToken => "bad_token",
            Self::UnknownCollection => "unknown_collection",
            Self::UserMissing => "user_missing",
            Self::Lookup => "lookup_error",
            Self::Locked => "account_locked",
            Self::StaleSession => "stale_session",
            Self::Unaccepted => "issuer_unaccepted",
        }
    }
}

/// Payload of [`Resolution::Authenticated`]. Boxed to keep the
/// `Resolution` enum small.
#[derive(Debug)]
pub struct AuthenticatedResolution {
    pub user: AuthUser,
    pub via: ResolvedMethod,
}

/// Which method on which collection actually produced the
/// authenticated principal. Useful for audit logging / debug
/// headers; callers don't need to act on it.
#[derive(Debug, Clone)]
pub enum ResolvedMethod {
    Bearer { collection: Slug },
    SessionCookie { collection: Slug },
    Strategy { collection: Slug, name: String },
}

/// Resolve a request to either a principal, Anonymous, or Invalid.
/// Pure over `(request, deps)` — safe to call from `spawn_blocking`.
///
/// # Precedence
///
/// Credentials are tried in this order:
///
/// 1. **Bearer token** (`Authorization: Bearer …` / gRPC metadata).
/// 2. **Session cookie** (`crap_session=…`, admin only).
/// 3. **Strategies**, walked across every auth collection (see
///    iteration-order caveat below).
///
/// The first credential that authenticates produces
/// `Resolution::Authenticated`. The first credential whose token
/// decodes but is *invalid* (bad signature, stale session, locked
/// user, missing user, unknown collection) **short-circuits** the
/// remaining paths and returns `Resolution::Invalid` immediately.
/// In practice a request never carries both a stale bearer and a
/// valid cookie (browsers don't send `Authorization`, gRPC clients
/// don't send cookies), but this precedence is by design: an
/// explicit credential that looks broken is suspicious and should
/// be surfaced, not silently bypassed.
///
/// # `Anonymous` vs `Invalid(Unaccepted)`
///
/// If no path authenticates and no path returned `Invalid`:
/// - **No credential was presented at all** → `Anonymous`. Caller
///   may proceed unauthenticated.
/// - **A credential was presented** but no method accepted it (the
///   issuing collection's `methods` list no longer covers the
///   credential type for this surface, and no strategy fired) →
///   `Invalid(Unaccepted)`. Caller must clear the stale credential
///   (admin: cookie clear + redirect to login; gRPC: 401) instead
///   of treating the request as silently anonymous, otherwise the
///   browser loops on the dead cookie.
///
/// # Iteration order — strategy walk
///
/// Cost is O(C × M) per request, where C = number of auth
/// collections and M = methods per collection. Bearer and cookie
/// paths short-circuit on the first valid token; the strategy walk
/// only invokes Lua hooks whose `surfaces` include the current
/// request *and* whose `activates_on` discriminator matches —
/// pure-Rust filters keep the per-request cost low even with many
/// strategies configured.
///
/// **Within a single collection**, methods are tried in declaration
/// order — first match wins, deterministic. **Across collections**,
/// iteration is `HashMap` order (`registry.collections` is a
/// `HashMap`), which is non-deterministic between runs. If two
/// collections both register strategies that would fire on the
/// same request, the winning collection is unpredictable.
///
/// The `crap-cms status` command warns about exactly two
/// collision shapes that hit this: multiple `Always`-active
/// strategies on the same surface, and multiple `Header`-activated
/// strategies bound to the same `(header, surface)` pair. Operators
/// who want predictable cross-collection precedence must keep each
/// strategy's activation signal unique.
pub fn evaluate(request: &AuthRequest<'_>, deps: &EvaluateDeps<'_>) -> Resolution {
    // Track whether the caller supplied any credential. If they
    // did and nothing authenticated by the end, we return
    // `Invalid(Unaccepted)` instead of `Anonymous` so the admin
    // middleware clears the now-dead cookie (preventing the
    // infinite redirect loop) and the gRPC handler returns 401
    // instead of treating the request as silently unauthenticated.
    let mut credential_supplied = false;

    // ── 1. Bearer ──────────────────────────────────────────────────
    if let Some(token) = request.bearer_token {
        credential_supplied = true;
        match resolve_token(token, request.surface, deps, Auth::accepts_bearer) {
            TokenOutcome::Authenticated(payload) => {
                let (user, slug) = *payload;
                return Resolution::Authenticated(Box::new(AuthenticatedResolution {
                    user,
                    via: ResolvedMethod::Bearer { collection: slug },
                }));
            }
            TokenOutcome::Invalid(failure) => return Resolution::Invalid(failure),
            TokenOutcome::NotAccepted => {}
        }
    }

    // ── 2. Session cookie ──────────────────────────────────────────
    if let Some(token) = request.session_cookie_token {
        credential_supplied = true;
        match resolve_token(token, request.surface, deps, Auth::accepts_session_cookie) {
            TokenOutcome::Authenticated(payload) => {
                let (user, slug) = *payload;
                return Resolution::Authenticated(Box::new(AuthenticatedResolution {
                    user,
                    via: ResolvedMethod::SessionCookie { collection: slug },
                }));
            }
            TokenOutcome::Invalid(failure) => return Resolution::Invalid(failure),
            TokenOutcome::NotAccepted => {}
        }
    }

    // ── 3. Strategies (cross-collection walk) ──────────────────────
    //
    // Fast-path: skip the entire walk when no collection declares a
    // strategy. Deployments that only use `password_login` / `bearer`
    // / `session_cookie` (the common case) pay zero per-request cost
    // for the strategy machinery — no collection iteration, no
    // method scan, no `activation_matches` checks. This is critical
    // for anonymous-read workloads where the token + cookie checks
    // both miss and we'd otherwise walk the registry on every
    // request to discover there is nothing to do.
    if !deps.registry.has_any_strategy() {
        return if credential_supplied {
            // Caller presented a credential but nothing accepted it.
            // Surface `Unaccepted` so the admin middleware clears
            // the dead cookie and gRPC returns 401 instead of
            // treating the request as anonymous.
            Resolution::Invalid(AuthFailure::Unaccepted)
        } else {
            Resolution::Anonymous
        };
    }

    // Strategy hooks have no per-call timeout: mlua doesn't expose
    // cooperative interruption at a stable enough API, and abandoning
    // the `spawn_blocking` thread (`tokio::time::timeout` around the
    // future) leaks the blocking worker until the strategy returns.
    // Operators MUST treat strategy code as untrusted only to the
    // degree they trust the Lua they shipped — a hostile or buggy
    // hook can hang the request. The validator warns on
    // `Always`-active strategies; for header-discriminated strategies
    // the precomputed `header_strategies` index already keeps slow
    // code off the hot path for requests that don't carry the
    // discriminator (no walk, just a HashMap lookup).
    //
    // Iteration order: always-active strategies first (matches the
    // pre-precompute walk's semantics for cross-collection precedence
    // — they always ran first by virtue of `activation_matches`
    // returning true unconditionally), then header-discriminated
    // strategies indexed by each request header that has a match.
    if let Some(entries) = deps.registry.always_strategies.get(&request.surface) {
        for entry in entries {
            if let Some(res) = try_strategy(entry, request, deps) {
                return res;
            }
        }
    }
    if !deps.registry.header_strategies.is_empty() {
        // Only iterate request headers when at least one
        // header-activated strategy exists in the registry. Each
        // header key is matched (lowercased) against the
        // `header_strategies` index — no walk, no allocation per
        // miss.
        for header_name in request.headers.keys() {
            let key = (header_name.to_ascii_lowercase(), request.surface);
            let Some(entries) = deps.registry.header_strategies.get(&key) else {
                continue;
            };
            for entry in entries {
                if let Some(res) = try_strategy(entry, request, deps) {
                    return res;
                }
            }
        }
    }

    if credential_supplied {
        // Caller proved knowledge of a valid token but every method
        // refused (issuer disabled, surfaces excluded, no strategy
        // matched). Treat as a hard failure so the cookie gets
        // cleared / the client gets a clear 401 instead of looping.
        return Resolution::Invalid(AuthFailure::Unaccepted);
    }
    Resolution::Anonymous
}

/// Try a single strategy entry against the current request. Returns
/// `Some(Resolution::Authenticated)` on a hit, `None` to keep
/// iterating. Mirrors the per-strategy `is_locked` / verify-email
/// guard the bearer / cookie paths apply in `resolve_token`, so the
/// strategy path can't be used to bypass account-state checks.
fn try_strategy(
    entry: &crate::core::StrategyEntry,
    request: &AuthRequest<'_>,
    deps: &EvaluateDeps<'_>,
) -> Option<Resolution> {
    let def = deps.registry.get_collection(&entry.slug)?;
    let auth = def.auth.as_ref()?;
    // Request-resolution path: identity comes from headers (SSO header, bearer,
    // etc.), so there are no submitted login credentials to expose.
    let strategy_input = AuthStrategyInput {
        collection: &entry.slug,
        headers: request.headers,
        email: None,
        password: None,
        remote_addr: None,
    };
    match deps
        .hook_runner
        .run_auth_strategy(&entry.authenticate, &strategy_input, deps.conn)
    {
        Ok(Some(doc)) => {
            if is_locked(&doc) {
                debug!(
                    collection = %entry.slug,
                    strategy = %entry.name,
                    user = %doc.id,
                    "strategy returned locked user; refusing"
                );
                return None;
            }
            if auth.requires_verify_email() && !is_verified(&doc) {
                debug!(
                    collection = %entry.slug,
                    strategy = %entry.name,
                    user = %doc.id,
                    "strategy returned unverified user (verify_email required); refusing"
                );
                return None;
            }
            let user = build_strategy_authuser(doc, &entry.slug, auth.token_expiry)?;
            Some(Resolution::Authenticated(Box::new(
                AuthenticatedResolution {
                    user,
                    via: ResolvedMethod::Strategy {
                        collection: entry.slug.clone(),
                        name: entry.name.clone(),
                    },
                },
            )))
        }
        Ok(None) => None,
        Err(e) => {
            warn!(
                collection = %entry.slug,
                strategy = %entry.name,
                error = ?e,
                "auth strategy returned error; continuing to next method"
            );
            None
        }
    }
}

/// Historical activation-matching helper. The runtime evaluator no
/// longer calls it — the precomputed `header_strategies` /
/// `always_strategies` indexes on `Registry` answer the same
/// question without iterating. Kept for the existing unit tests
/// that pin the case-insensitive matching semantics; if those tests
/// move to exercising the precomputed maps directly, this can go.
#[cfg(test)]
fn activation_matches(
    activation: &crate::core::collection::Activation,
    headers: &HashMap<String, String>,
) -> bool {
    use crate::core::collection::Activation;
    match activation {
        Activation::Always { .. } => true,
        Activation::Header { header } => {
            let want = header.as_str();
            headers.keys().any(|k| k.eq_ignore_ascii_case(want))
        }
    }
}

enum TokenOutcome {
    Authenticated(Box<(AuthUser, Slug)>),
    Invalid(AuthFailure),
    /// Token decoded fine but the issuing collection doesn't list
    /// the requested credential type for this surface. Caller
    /// continues to the next credential / strategy path rather
    /// than returning an error — the holder may still authenticate
    /// via a different method.
    NotAccepted,
}

fn resolve_token<F>(
    token: &str,
    surface: Surface,
    deps: &EvaluateDeps<'_>,
    accepts: F,
) -> TokenOutcome
where
    F: Fn(&Auth, Surface) -> bool,
{
    let Ok(claims) = deps.token_provider.validate_token(token) else {
        debug!(surface = ?surface, "token validation failed");
        return TokenOutcome::Invalid(AuthFailure::BadToken);
    };

    let Some(def) = deps.registry.get_collection(&claims.collection) else {
        debug!(
            collection = %claims.collection,
            "token references unknown auth collection"
        );
        return TokenOutcome::Invalid(AuthFailure::UnknownCollection);
    };

    let Some(auth) = def.auth.as_ref() else {
        return TokenOutcome::NotAccepted;
    };
    if !auth.enabled || !accepts(auth, surface) {
        return TokenOutcome::NotAccepted;
    }

    let doc = match query::find_by_id(deps.conn, &claims.collection, def, &claims.sub, None) {
        Ok(Some(d)) => d,
        Ok(None) => {
            debug!(user = %claims.sub, collection = %claims.collection, "user missing");
            return TokenOutcome::Invalid(AuthFailure::UserMissing);
        }
        Err(e) => {
            debug!(error = ?e, "user lookup failed");
            return TokenOutcome::Invalid(AuthFailure::Lookup);
        }
    };

    if is_locked(&doc) {
        return TokenOutcome::Invalid(AuthFailure::Locked);
    }

    let ctx = ServiceContext::slug_only(&claims.collection)
        .conn(deps.conn)
        .build();
    let Ok(db_session_version) = service::auth::get_session_version(&ctx, &claims.sub) else {
        debug!(user = %claims.sub, "session version lookup failed");
        return TokenOutcome::Invalid(AuthFailure::Lookup);
    };
    if claims.session_version != db_session_version {
        return TokenOutcome::Invalid(AuthFailure::StaleSession);
    }

    TokenOutcome::Authenticated(Box::new((AuthUser::new(claims, doc), def.slug.clone())))
}

fn is_locked(doc: &Document) -> bool {
    bool_flag(doc, "_locked")
}

fn is_verified(doc: &Document) -> bool {
    bool_flag(doc, "_verified")
}

/// Defensive reader for the `_locked` / `_verified` integer flags
/// on a user document. Accepts:
/// - `i64` (the canonical DB-backed shape: 0 = false, anything else = true)
/// - `bool` (in case a Lua hook returns a boolean directly)
/// - string-coercible-to-i64 ("0", "1", etc.) — strategy hooks
///   synthesizing docs occasionally end up with stringy values.
///
/// Anything else parses as `false` (the safe default: don't grant
/// special-case access on the missing-true side; do refuse on the
/// locked/unverified side only when we can prove it).
fn bool_flag(doc: &Document, key: &str) -> bool {
    let Some(value) = doc.fields.get(key) else {
        return false;
    };
    if let Some(i) = value.as_i64() {
        return i != 0;
    }
    if let Some(b) = value.as_bool() {
        return b;
    }
    if let Some(s) = value.as_str() {
        // Shared truthy set (`1/true/yes/on`, case-insensitive). The old ad-hoc
        // set here missed `on` and only case-folded `true`, so a `_locked = "on"`
        // read as NOT locked — a fail-open. Route through the one chokepoint.
        return parse_truthy(s);
    }
    false
}

/// Materialize an [`AuthUser`] from already-validated [`Claims`]:
/// look up the user document, reject locked accounts, reject
/// stale session versions. Returns `None` for any failure mode —
/// callers that have a Claims in hand (MFA completion, login
/// callbacks) don't need the granular [`AuthFailure`] reasons that
/// `resolve_token` produces because they just authenticated the
/// user a moment ago; a stale lookup here is genuinely unexpected
/// rather than user-visible.
///
/// Shared between [`resolve_token`] and the admin middleware's
/// `load_auth_user` so the locked / stale-session checks stay in
/// one place.
#[cfg(not(tarpaulin_include))]
pub fn load_authenticated_user(
    claims: &Claims,
    registry: &Registry,
    conn: &dyn DbConnection,
) -> Option<AuthUser> {
    let def = registry.get_collection(&claims.collection)?;
    let doc = query::find_by_id(conn, &claims.collection, def, &claims.sub, None)
        .ok()
        .flatten()?;

    if is_locked(&doc) {
        return None;
    }

    let ctx = ServiceContext::slug_only(&claims.collection)
        .conn(conn)
        .build();
    let db_session_version = service::auth::get_session_version(&ctx, &claims.sub).ok()?;
    if claims.session_version != db_session_version {
        return None;
    }

    Some(AuthUser::new(claims.clone(), doc))
}

/// Build claims + `AuthUser` for a strategy-authenticated request.
///
/// Strategy auth doesn't issue a JWT to the client — the produced
/// `Claims` are internal-only, populated to keep the downstream
/// shape identical to the bearer / cookie paths (request
/// extensions, hook user context, audit). The `exp` field is set
/// from the collection's `token_expiry` for consistency but isn't
/// re-validated; treating it as informative metadata.
///
/// Refuses (returns `None`) any doc with an empty `id` — a strategy
/// hook returning such a doc is operator error, and propagating an
/// empty `sub` claim downstream would silently break session-
/// version lookups and audit logging.
fn build_strategy_authuser(doc: Document, slug: &Slug, token_expiry: u64) -> Option<AuthUser> {
    if doc.id.is_empty() {
        warn!(collection = %slug, "strategy returned document with empty id; refusing");
        return None;
    }
    let now = u64::try_from(Utc::now().timestamp().max(0)).unwrap_or(0);
    let email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let claims: Claims = match ClaimsBuilder::new(doc.id.clone(), slug.clone())
        .email(email)
        .exp(now.saturating_add(token_expiry))
        .auth_time(now)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(collection = %slug, error = ?e, "strategy claims build failed");
            return None;
        }
    };
    Some(AuthUser::new(claims, doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::{Activation, Auth, AuthMethod, SurfaceSet};

    #[test]
    fn activation_always_always_fires() {
        let headers = HashMap::new();
        assert!(activation_matches(&Activation::always(), &headers));
    }

    #[test]
    fn activation_header_fires_only_when_header_present() {
        let mut headers = HashMap::new();
        let act = Activation::Header {
            header: "X-Api-Key".to_string(),
        };
        assert!(!activation_matches(&act, &headers));
        headers.insert("x-api-key".to_string(), "k".to_string());
        assert!(activation_matches(&act, &headers));
    }

    #[test]
    fn activation_header_case_insensitive_lookup() {
        let act = Activation::Header {
            header: "X-API-KEY".to_string(),
        };
        // Lowercase header in the request matches uppercase
        // discriminator.
        let mut lower = HashMap::new();
        lower.insert("x-api-key".to_string(), "k".to_string());
        assert!(activation_matches(&act, &lower));

        // Mixed-case header in the request also matches — preserves
        // the contract of Lua hooks seeing original-cased keys while
        // activation matching stays case-insensitive.
        let mut mixed = HashMap::new();
        mixed.insert("X-Api-Key".to_string(), "k".to_string());
        assert!(activation_matches(&act, &mixed));
    }

    #[test]
    fn auth_accepts_bearer_honors_surfaces() {
        let auth = Auth {
            enabled: true,
            methods: vec![AuthMethod::Bearer {
                surfaces: SurfaceSet::admin_only(),
            }],
            ..Default::default()
        };
        assert!(auth.accepts_bearer(Surface::Admin));
        assert!(!auth.accepts_bearer(Surface::Grpc));
    }

    #[test]
    fn auth_accepts_session_cookie_honors_surfaces() {
        let auth = Auth {
            enabled: true,
            methods: vec![AuthMethod::SessionCookie {
                surfaces: SurfaceSet::admin_only(),
            }],
            ..Default::default()
        };
        assert!(auth.accepts_session_cookie(Surface::Admin));
        assert!(!auth.accepts_session_cookie(Surface::Grpc));
    }

    #[test]
    fn is_locked_reads_dunder_locked_field() {
        let mut doc = Document::new("u1".to_string());
        assert!(!is_locked(&doc));
        doc.fields
            .insert("_locked".to_string(), serde_json::json!(1));
        assert!(is_locked(&doc));
        doc.fields
            .insert("_locked".to_string(), serde_json::json!(0));
        assert!(!is_locked(&doc));
    }

    #[test]
    fn is_verified_reads_dunder_verified_field() {
        let mut doc = Document::new("u1".to_string());
        assert!(!is_verified(&doc));
        doc.fields
            .insert("_verified".to_string(), serde_json::json!(1));
        assert!(is_verified(&doc));
    }

    /// Defensive parsing: a strategy hook synthesizing a doc with
    /// `_locked = "1"` (string) or `_locked = true` (bool) must
    /// still register as locked — otherwise the lock check is a
    /// silent no-op for that path.
    #[test]
    fn bool_flag_accepts_string_and_bool_shapes() {
        let mut doc = Document::new("u1".to_string());
        // String "1" / "true" — locked.
        doc.fields
            .insert("_locked".to_string(), serde_json::json!("1"));
        assert!(is_locked(&doc));
        doc.fields
            .insert("_locked".to_string(), serde_json::json!("true"));
        assert!(is_locked(&doc));
        // Bool true — locked.
        doc.fields
            .insert("_locked".to_string(), serde_json::json!(true));
        assert!(is_locked(&doc));
        // String "0" / "false" / "" — not locked.
        doc.fields
            .insert("_locked".to_string(), serde_json::json!("0"));
        assert!(!is_locked(&doc));
        doc.fields
            .insert("_locked".to_string(), serde_json::json!("false"));
        assert!(!is_locked(&doc));
        // Other types fall back to false (the safe default for
        // `is_locked`; the strategy gets through, but a separate
        // check via DB query would catch a real lock).
        doc.fields
            .insert("_locked".to_string(), serde_json::json!(null));
        assert!(!is_locked(&doc));
    }

    #[test]
    fn build_strategy_authuser_refuses_empty_id() {
        let slug = Slug::new("users");
        let doc = Document::new(String::new());
        assert!(
            build_strategy_authuser(doc, &slug, 7200).is_none(),
            "doc with empty id must be refused"
        );
    }

    #[test]
    fn build_strategy_authuser_accepts_well_formed_doc() {
        let slug = Slug::new("users");
        let mut doc = Document::new("u1".to_string());
        doc.fields
            .insert("email".to_string(), serde_json::json!("a@x.com"));
        let user = build_strategy_authuser(doc, &slug, 7200).expect("well-formed doc");
        assert_eq!(user.claims.sub, "u1");
        assert_eq!(user.claims.email, "a@x.com");
    }
}
