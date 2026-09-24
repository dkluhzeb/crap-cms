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

use crate::config::LocaleConfig;
use crate::core::{
    AuthUser, Claims, CollectionDefinition, Document, Registry, Slug, StrategyEntry,
    auth::{ClaimsBuilder, TokenProvider, TokenUse},
    collection::{Auth, Surface},
};
use crate::db::{DbConnection, query};
use crate::hooks::{HookRunner, lifecycle::AuthStrategyInput};
use crate::service::{
    AppInfra, ServiceContext,
    auth::{StrategyAdmission, admit_strategy_user, load_user},
};

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
    /// Needed to read the user row of a LOCALIZED auth collection (a bare
    /// column list errors there).
    pub locale_config: &'a LocaleConfig,
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
/// iterating. A locked account — or an unverified one where the
/// collection requires verification — is refused, so the strategy path
/// can't be used to bypass the account-state checks the bearer / cookie
/// paths apply.
fn try_strategy(
    entry: &StrategyEntry,
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

    let doc =
        match deps
            .hook_runner
            .run_auth_strategy(&entry.authenticate, &strategy_input, deps.conn)
        {
            Ok(Some(doc)) => doc,
            Ok(None) => return None,
            Err(e) => {
                warn!(
                    collection = %entry.slug,
                    strategy = %entry.name,
                    error = ?e,
                    "auth strategy returned error; continuing to next method"
                );
                return None;
            }
        };

    let ctx = user_ctx(def, deps.conn, deps.locale_config);
    let (user, session_version) = admitted_strategy_user(&ctx, &doc, auth, &entry.name)?;
    let user = build_strategy_authuser(user, session_version, &entry.slug, auth.token_expiry)?;

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

/// A context for reading a user of the auth collection `def`.
fn user_ctx<'a>(
    def: &'a CollectionDefinition,
    conn: &'a dyn DbConnection,
    locale_config: &'a LocaleConfig,
) -> ServiceContext<'a> {
    ServiceContext::collection(&def.slug, def)
        .conn(conn)
        .locale_config(Some(locale_config))
        .build()
}

/// The stored document of the user a strategy's hook named, admitted through
/// [`admit_strategy_user`] exactly as the login path and the external auth
/// callback admit one: the stored row decides lock / verification state, the
/// hook's table may only restrict it, and the session is built from the
/// stored document (a Lua table can't carry a NULL field or the hidden ones,
/// so access rules would otherwise judge a different `ctx.user` than for the
/// same user signed in with a token). Returns the stored document with its
/// current session version. `None` — refusing the user — on any refusal or a
/// failed read (fail closed).
fn admitted_strategy_user(
    ctx: &ServiceContext<'_>,
    doc: &Document,
    auth: &Auth,
    strategy: &str,
) -> Option<(Document, u64)> {
    match admit_strategy_user(ctx, doc, auth.requires_verify_email()) {
        Ok(StrategyAdmission::Admitted {
            user,
            session_version,
        }) => Some((user, session_version)),
        Ok(StrategyAdmission::Refused(refusal)) => {
            debug!(
                collection = %ctx.slug,
                strategy = %strategy,
                user = %doc.id,
                "strategy returned a {} user; refusing",
                refusal.as_str()
            );

            None
        }
        Err(e) => {
            warn!(
                collection = %ctx.slug,
                strategy = %strategy,
                user = %doc.id,
                error = %e,
                "account state lookup failed; refusing the strategy's user"
            );

            None
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

    let ctx = user_ctx(def, deps.conn, deps.locale_config);
    let doc = match load_user(&ctx, &claims.sub) {
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

    if let Err(failure) = check_account(deps.conn, &claims) {
        return TokenOutcome::Invalid(failure);
    }

    TokenOutcome::Authenticated(Box::new((AuthUser::new(claims, doc), def.slug.clone())))
}

/// Whether the account a token names may still use it: not locked, and on the
/// session version the token was issued under. Both are read from the row in
/// one query — a user document never carries `_locked`.
fn check_account(conn: &dyn DbConnection, claims: &Claims) -> Result<(), AuthFailure> {
    let (locked, db_session_version) =
        query::lock_and_session_version(conn, &claims.collection, &claims.sub)
            .inspect_err(|e| debug!(user = %claims.sub, error = ?e, "account lookup failed"))
            .map_err(|_| AuthFailure::Lookup)?;

    if locked {
        return Err(AuthFailure::Locked);
    }

    if claims.session_version != db_session_version {
        return Err(AuthFailure::StaleSession);
    }

    Ok(())
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
/// Shared by [`reload_authenticated_user`] and the admin `load_auth_user` so
/// the locked / stale-session checks stay in one place; [`resolve_token`]
/// applies the same checks to a presented token.
#[cfg(not(tarpaulin_include))]
pub fn load_authenticated_user(
    claims: &Claims,
    registry: &Registry,
    conn: &dyn DbConnection,
    locale_config: &LocaleConfig,
) -> Option<AuthUser> {
    let def = registry.get_collection(&claims.collection)?;
    let doc = load_user(&user_ctx(def, conn, locale_config), &claims.sub)
        .ok()
        .flatten()?;

    check_account(conn, claims).ok()?;

    Some(AuthUser::new(claims.clone(), doc))
}

/// Re-resolve an [`AuthUser`] from validated claims against a fresh pooled
/// connection, fail-closed. This is the single pooled wrapper both MFA
/// completion paths (gRPC `VerifyMfa` and admin `verify_mfa_action`) call
/// before minting a session, so a lock / delete / session-version bump inside
/// the pending-MFA window invalidates the challenge on every surface. Returns
/// `None` on pool-acquire failure or any fail-closed reason from
/// [`load_authenticated_user`].
#[must_use]
pub fn reload_authenticated_user(infra: &AppInfra, claims: &Claims) -> Option<AuthUser> {
    let conn = infra.pool.get().ok()?;

    load_authenticated_user(claims, &infra.registry, &conn, &infra.locale_config)
}

/// Build claims + `AuthUser` for a strategy-authenticated request.
///
/// Strategy auth doesn't issue a JWT to the client — the produced
/// `Claims` are internal-only, populated to keep the downstream
/// shape identical to the bearer / cookie paths (request
/// extensions, hook user context, audit). They carry
/// [`TokenUse::Strategy`], so the token provider refuses to sign them and
/// no handler can mistake them for a session's claims. They carry the
/// user's stored `session_version` (read when the user was admitted), so
/// work the request queues is revoked by a later session-version bump
/// exactly like a token user's. The `exp` field is set from the
/// collection's `token_expiry` for consistency but isn't re-validated;
/// treating it as informative metadata.
///
/// Refuses (returns `None`) any doc with an empty `id` — a strategy
/// hook returning such a doc is operator error, and propagating an
/// empty `sub` claim downstream would silently break session-
/// version lookups and audit logging.
fn build_strategy_authuser(
    doc: Document,
    session_version: u64,
    slug: &Slug,
    token_expiry: u64,
) -> Option<AuthUser> {
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
        .session_version(session_version)
        .token_use(TokenUse::Strategy)
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
    use serde_json::json;

    use super::*;
    use crate::core::collection::{Activation, AuthMethod, SurfaceSet};

    /// Regression: strategy claims were indistinguishable from a session's
    /// (`token_use = Session`, `session_version = 0`), so the admin
    /// session-refresh endpoint exchanged a strategy credential for a signed
    /// session JWT.
    #[test]
    fn strategy_claims_are_marked_as_strategy_claims() {
        let doc = Document::builder("m1").build();

        let user = build_strategy_authuser(doc, 0, &Slug::new("members"), 3600).unwrap();

        assert_eq!(user.claims.token_use, TokenUse::Strategy);
    }

    /// Regression: strategy claims always carried `session_version = 0`, so
    /// work queued under them could not be revoked by a session-version
    /// bump — they now carry the stored version the user was admitted with.
    #[test]
    fn strategy_claims_carry_the_stored_session_version() {
        let doc = Document::builder("m1").build();

        let user = build_strategy_authuser(doc, 7, &Slug::new("members"), 3600).unwrap();

        assert_eq!(user.claims.session_version, 7);
    }

    /// Activation matching as the precomputed `header_strategies` /
    /// `always_strategies` indexes on `Registry` answer it — pins the
    /// case-insensitive header semantics.
    fn activation_matches(activation: &Activation, headers: &HashMap<String, String>) -> bool {
        match activation {
            Activation::Always { .. } => true,
            Activation::Header { header } => {
                let want = header.as_str();
                headers.keys().any(|k| k.eq_ignore_ascii_case(want))
            }
        }
    }

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
    fn build_strategy_authuser_refuses_empty_id() {
        let slug = Slug::new("users");
        let doc = Document::new(String::new());
        assert!(
            build_strategy_authuser(doc, 0, &slug, 7200).is_none(),
            "doc with empty id must be refused"
        );
    }

    #[test]
    fn build_strategy_authuser_accepts_well_formed_doc() {
        let slug = Slug::new("users");
        let mut doc = Document::new("u1".to_string());
        doc.fields.insert("email".to_string(), json!("a@x.com"));
        let user = build_strategy_authuser(doc, 0, &slug, 7200).expect("well-formed doc");
        assert_eq!(user.claims.sub, "u1");
        assert_eq!(user.claims.email, "a@x.com");
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod strategy_user_tests {
    use mlua::Lua;
    use rusqlite::Connection;
    use serde_json::Value;

    use super::*;
    use crate::{
        core::{FieldDefinition, FieldType, HookRef},
        db::AccessResult,
        hooks::lifecycle::{AccessCheckInput, access::check_collection_access},
    };

    /// A `users` auth collection with a `tenant_id` field, holding `u1`
    /// whose `tenant_id` is NULL.
    fn tenantless_users() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                email TEXT,
                tenant_id TEXT,
                _locked INTEGER DEFAULT 0,
                _verified INTEGER DEFAULT 0,
                _session_version INTEGER DEFAULT 0,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, email) VALUES ('u1', 'u1@x.com');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::new(true));
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email).build(),
            FieldDefinition::builder("tenant_id", FieldType::Text).build(),
        ];

        (conn, def)
    }

    /// `{ tenant_id = ctx.user.tenant_id, archived = false }` for `user`.
    fn tenant_rule(user: &Document) -> AccessResult {
        let lua = Lua::new();
        lua.load(
            r#"package.loaded["rules"] = {
                tenant = function(ctx)
                    return { tenant_id = ctx.user.tenant_id, archived = false }
                end,
            }"#,
        )
        .exec()
        .unwrap();

        check_collection_access(
            &lua,
            &AccessCheckInput::builder("find", "posts")
                .access(Some(&HookRef::new("rules.tenant")))
                .user(Some(user))
                .build(),
        )
        .unwrap()
    }

    /// Regression: a strategy's user reached access rules as the table the
    /// strategy returned, where a NULL field's key is dropped — so the NULL-read
    /// guard had nothing to guard and a tenant rule widened to
    /// `{ archived = false }`, every tenant's rows. The user is now the stored
    /// document, read like a token user's, and the rule is denied.
    #[test]
    fn a_strategy_user_with_a_null_tenant_is_denied_a_tenant_constraint() {
        let (conn, def) = tenantless_users();
        let locale_config = LocaleConfig::default();
        let ctx = user_ctx(&def, &conn, &locale_config);

        // What the strategy's Lua table carries: no `tenant_id` key at all.
        let returned = Document::builder("u1").build();
        assert!(
            matches!(tenant_rule(&returned), AccessResult::Constrained(_)),
            "the returned table alone widens the rule"
        );

        let auth = def.auth.as_ref().unwrap();
        let (stored, _) =
            admitted_strategy_user(&ctx, &returned, auth, "sso").expect("stored user");
        assert_eq!(stored.fields.get("tenant_id"), Some(&Value::Null));

        let result = tenant_rule(&stored);
        assert!(matches!(result, AccessResult::Denied), "{result:?}");
    }

    /// A strategy may only name a stored user of its collection.
    #[test]
    fn a_strategy_user_that_is_not_stored_is_refused() {
        let (conn, def) = tenantless_users();
        let locale_config = LocaleConfig::default();
        let ctx = user_ctx(&def, &conn, &locale_config);

        let synthesized = Document::builder("nobody").build();

        let auth = def.auth.as_ref().unwrap();

        assert!(admitted_strategy_user(&ctx, &synthesized, auth, "sso").is_none());
    }

    /// The admitted user comes with the stored session version, which the
    /// strategy claims carry so a later bump revokes work queued under them.
    #[test]
    fn a_strategy_user_is_admitted_with_the_stored_session_version() {
        let (conn, def) = tenantless_users();
        conn.execute("UPDATE users SET _session_version = 3 WHERE id = 'u1'", [])
            .unwrap();
        let locale_config = LocaleConfig::default();
        let ctx = user_ctx(&def, &conn, &locale_config);

        let returned = Document::builder("u1").build();
        let auth = def.auth.as_ref().unwrap();

        let (_, session_version) =
            admitted_strategy_user(&ctx, &returned, auth, "sso").expect("stored user");

        assert_eq!(session_version, 3);
    }
}
