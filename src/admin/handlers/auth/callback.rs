//! Auth callback handler — dispatches `/admin/auth/callback/{name}` to Lua hooks.
//!
//! Enables OAuth2/OIDC and external auth providers implemented entirely in Lua.
//! Reached by a `GET` redirect or, for an identity provider using
//! `response_mode=form_post`, a cross-site `POST` of a urlencoded form (the
//! callback routes are exempt from the double-submit CSRF check; the OAuth
//! `state` the hook verifies is their login-CSRF defense). The hook receives
//! one `ctx.headers` map (see [`hook_context`]); it returns a user document
//! to create a session (after the collection's MFA step, unless the
//! collection exempts the callback), or nil to redirect to login.

use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use anyhow::anyhow;
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, Method},
    response::{IntoResponse, Redirect, Response},
};
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        auth_middleware::check_admin_gate_for_doc,
        csrf::is_form_urlencoded,
        handlers::{
            auth::{
                client_ip, create_session_token, extract_user_email, issue_mfa_challenge,
                refusal_error_key, session_redirect, sole_auth_collection,
            },
            shared::paths,
        },
        server::headers_to_map,
    },
    core::{
        Builder, Document, HookRef,
        collection::{Auth, Surface},
        spawn_request_blocking,
    },
    hooks::lifecycle::AuthStrategyInput,
    service::{
        AppInfra, ServiceContext,
        auth::{
            LoginOutcome, LoginVerified, MfaGateRequest, SessionGrant, StrategyAdmission,
            admit_strategy_user, mfa_gate,
        },
    },
};

/// One auth-callback request, resolved to the auth `collection` its session
/// may bind to.
#[derive(Builder)]
pub(super) struct CallbackRequest<'a> {
    /// The peer address (the rate limiter's key, via [`client_ip`]).
    #[builder(required)]
    addr: SocketAddr,
    /// The only collection the session may bind to.
    #[builder(required)]
    collection: &'a str,
    /// The callback name: the hook run is `auth_callback.{name}`.
    #[builder(required)]
    name: &'a str,
    /// The HTTP method (`GET`, or `POST` for a `form_post` response).
    #[builder(required)]
    method: &'a Method,
    /// The URL query parameters.
    #[builder(required)]
    params: &'a HashMap<String, String>,
    /// The fields of a urlencoded form body (empty without one).
    #[builder(required)]
    form: &'a HashMap<String, String>,
    /// The request headers.
    #[builder(required)]
    headers: &'a HeaderMap,
}

/// Key prefix of a query parameter in the hook context.
const QUERY_PREFIX: &str = "_query_";

/// Key prefix of a form-body field in the hook context.
const FORM_PREFIX: &str = "_form_";

/// Key of the HTTP method in the hook context.
const METHOD_KEY: &str = "_method";

/// The fields of a urlencoded form body — an identity provider's
/// `response_mode=form_post` answer (`code`, `state`, `id_token`, …). Empty
/// for any other body. Shared by both callback routes.
pub(super) fn form_fields(headers: &HeaderMap, body: &Bytes) -> HashMap<String, String> {
    if !is_form_urlencoded(headers) {
        return HashMap::new();
    }

    form_urlencoded::parse(body).into_owned().collect()
}

/// The redirect every refused or failed callback answers with.
fn login_redirect() -> Response {
    Redirect::to(paths::LOGIN).into_response()
}

/// Whether a header name collides with a key the framework sets.
fn reserved_key(name: &str) -> bool {
    name.starts_with(QUERY_PREFIX) || name.starts_with(FORM_PREFIX) || name == METHOD_KEY
}

/// The hook's view of the request (`ctx.headers`): every request header,
/// each query parameter as `_query_{name}`, each urlencoded form-body field
/// as `_form_{name}`, and the HTTP method as `_method`. A request header
/// spelled like one of those keys is dropped, so each can only come from
/// where its name says.
fn hook_context(request: &CallbackRequest) -> HashMap<String, String> {
    let mut ctx = headers_to_map(request.headers);

    ctx.retain(|name, _| !reserved_key(name));

    for (k, v) in request.params {
        ctx.insert(format!("{QUERY_PREFIX}{k}"), v.clone());
    }

    for (k, v) in request.form {
        ctx.insert(format!("{FORM_PREFIX}{k}"), v.clone());
    }

    ctx.insert(METHOD_KEY.to_string(), request.method.to_string());

    ctx
}

/// Execute the configured Lua auth callback hook for an external auth flow
/// (OAuth callback etc.). The hook commonly provisions the user it names
/// (create on first sign-in), committed with the authentication; its write
/// connection is taken only at its first CRUD call, so the provider round
/// trips before it hold none (see [`HookRunner::run_auth_callback`]).
///
/// [`HookRunner::run_auth_callback`]: crate::hooks::HookRunner::run_auth_callback
fn run_auth_strategy_blocking(
    infra: &AppInfra,
    hook_ref: &str,
    collection: &str,
    ctx: &HashMap<String, String>,
) -> anyhow::Result<Option<Document>> {
    let input = AuthStrategyInput {
        collection,
        headers: ctx,
        email: None,
        password: None,
        remote_addr: None,
    };
    // OAuth-callback hook refs are synthesized (`auth_callback.<name>`), not
    // config-declared, so they carry no per-config options.
    let strategy = HookRef::new(hook_ref);

    infra
        .hook_runner
        .run_auth_callback(&strategy, &input, infra)
        .map_err(|e| anyhow!("Auth callback hook error: {e:#}"))
}

/// Run the Lua auth callback hook in a blocking task. `None` when the hook
/// returned nil, failed, or its task panicked — each refuses the callback.
async fn run_auth_callback_hook(
    state: &AdminState,
    request: &CallbackRequest<'_>,
) -> Option<Document> {
    let infra = Arc::clone(&state.infra);
    let hook_ref = format!("auth_callback.{}", request.name);
    let collection = request.collection.to_string();
    let ctx = hook_context(request);

    let result = spawn_request_blocking(move || {
        run_auth_strategy_blocking(&infra, &hook_ref, &collection, &ctx)
    })
    .await
    .inspect_err(|e| error!("Auth callback task error: {}", e))
    .ok()?;

    result
        .inspect_err(|e| error!("Auth callback error: {:#}", e))
        .ok()
        .flatten()
}

/// Owned inputs for the callback admission `spawn_blocking` body. All fields
/// required; built in one place — plain struct literal.
struct CallbackAdmission {
    infra: Arc<AppInfra>,
    /// The collection the callback runs under.
    slug: String,
    /// The callback name (checked against the collection's MFA exemptions).
    name: String,
    /// The request headers, exposed to the `mfa_when` gate.
    headers: HashMap<String, String>,
    /// The user table the hook returned.
    hook_doc: Document,
}

/// Admit the user an auth-callback hook named into the request's collection,
/// exactly as a custom-strategy login admits one ([`admit_strategy_user`]),
/// then route it through the collection's MFA gate ([`mfa_gate`]) — the same
/// gate a password or strategy login passes, so a callback cannot skip the
/// second factor unless the collection exempts it by name.
///
/// `slug` is the collection the callback runs under — the **only** collection a
/// callback session may bind to. The session is bound here, NOT to whatever
/// collection happens to contain the id: binding by id across collections
/// would let a hook-returned (or OAuth-provider-influenced) id mint a session
/// for a *different*, possibly higher-privilege, auth collection. So the user
/// must be a stored, non-trashed row of `slug`.
///
/// Refuses (`None`) an id naming no such user, a locked account, and — when
/// the collection requires email verification — an unverified one; the hook's
/// table may only restrict those flags. Every lookup fails CLOSED: a transient
/// DB error never mints a session. The session and the `admin.access` gate
/// then work from the **stored** document, never the hook's table.
fn admit_callback_user(input: &CallbackAdmission) -> Option<LoginOutcome> {
    let infra = &input.infra;
    let def = infra.registry.get_collection(&input.slug)?;
    let conn = infra
        .pool
        .get()
        .inspect_err(|e| error!("Auth callback: DB connection: {e:#}"))
        .ok()?;
    let ctx = ServiceContext::collection(&input.slug, def)
        .conn(&conn)
        .locale_config(Some(&infra.locale_config))
        .build();

    let (user, session_version) = callback_admission(&ctx, &input.hook_doc)?;

    let gate = MfaGateRequest::builder(&input.slug, def, Surface::Admin, &input.headers)
        .callback(Some(input.name.as_str()))
        .build();
    let verified = LoginVerified::builder(user, session_version).build();

    Some(mfa_gate(infra, &conn, &gate, verified))
}

/// [`admit_callback_user`] against a built collection context.
fn callback_admission(ctx: &ServiceContext, hook_doc: &Document) -> Option<(Document, u64)> {
    let require_verified = ctx
        .collection_def()
        .ok()?
        .auth
        .as_ref()
        .is_some_and(Auth::requires_verify_email);

    match admit_strategy_user(ctx, hook_doc, require_verified) {
        Ok(StrategyAdmission::Admitted {
            user,
            session_version,
        }) => Some((user, session_version)),
        Ok(StrategyAdmission::Refused(refusal)) => {
            warn!(
                collection = %ctx.slug,
                user = %hook_doc.id,
                "auth callback named a {} user; refusing",
                refusal.as_str()
            );

            None
        }
        Err(e) => {
            error!("Auth callback account lookup failed: {e}");

            None
        }
    }
}

/// Authenticate a callback: run its hook, then admit the named user into the
/// request's collection and judge its MFA requirement off the async runtime
/// ([`admit_callback_user`]). `None` refuses the callback.
async fn authenticate_callback(
    state: &AdminState,
    request: &CallbackRequest<'_>,
) -> Option<LoginOutcome> {
    let hook_doc = run_auth_callback_hook(state, request).await?;

    let input = CallbackAdmission {
        infra: Arc::clone(&state.infra),
        slug: request.collection.to_string(),
        name: request.name.to_string(),
        headers: headers_to_map(request.headers),
        hook_doc,
    };

    spawn_request_blocking(move || admit_callback_user(&input))
        .await
        .inspect_err(|e| error!("Auth callback admission task error: {}", e))
        .ok()
        .flatten()
}

/// Mint the session for an admitted user of `collection` and redirect into
/// the admin.
fn callback_session_response(
    state: &AdminState,
    collection: &str,
    verified: &LoginVerified,
) -> Response {
    let user = &verified.user;
    let email = extract_user_email(user);
    let grant = SessionGrant::builder(
        &user.id,
        collection,
        &email,
        verified.session_version,
        Surface::Admin,
    )
    .mfa(verified.mfa);

    let Ok(session) =
        create_session_token(state, grant).inspect_err(|e| error!("Auth callback: {}", e))
    else {
        return login_redirect();
    };

    session_redirect(state, &session)
}

/// Finish an admitted callback: the `admin.access` gate, then either the MFA
/// challenge (the collection requires the second factor for this callback)
/// or the session.
///
/// The `admin.access` gate gives parity with the password login path: without
/// it a denied user still gets a valid session cookie and is only stopped by
/// the middleware on first page load (a useless cookie + 403 instead of a
/// clean denial).
async fn finish_callback(
    state: &AdminState,
    collection: &str,
    verified: LoginVerified,
    mfa: bool,
) -> Response {
    if let Some(response) = check_admin_gate_for_doc(state, &verified.user).await {
        return response;
    }

    if !mfa {
        return callback_session_response(state, collection, &verified);
    }

    let email = extract_user_email(&verified.user);

    issue_mfa_challenge(state, collection, &verified, &email).unwrap_or_else(|refusal| {
        warn!(
            collection,
            reason = refusal_error_key(refusal),
            "auth callback MFA challenge refused"
        );

        login_redirect()
    })
}

/// Run the rate-limit → authenticate → gate → MFA / mint-session flow for a
/// callback that has already resolved its target auth collection. Shared by
/// the legacy un-scoped handler ([`auth_callback`]) and the collection-scoped
/// handler ([`super::callback_scoped::auth_callback_scoped`]).
///
/// The session binds ONLY to the request's collection: the hook-returned user
/// must exist in it (enforced by [`admit_callback_user`]), so a returned id
/// that lives only in another auth collection is refused — no
/// cross-collection (privilege-escalating) binding regardless of which route
/// reached here.
pub(super) async fn complete_auth_callback(
    state: &AdminState,
    request: &CallbackRequest<'_>,
) -> Response {
    let client = client_ip(request.headers, &request.addr, &state.config.server);

    // Atomically record this callback attempt against the IP limiter and bail
    // if over threshold — one operation, closing the burst race the is_blocked
    // + later record_failure split left open. A successful login refunds it below.
    if state.ip_login_limiter.check_and_block_ip(&client) {
        return login_redirect();
    }

    // The returned user must be stored in the collection; lock + verify-email
    // enforced, then the collection's MFA gate. Everything below works from
    // the stored document.
    let (verified, mfa) = match authenticate_callback(state, request).await {
        Some(LoginOutcome::Verified(v)) => (v, false),
        Some(LoginOutcome::MfaRequired(v)) => (v, true),
        Some(LoginOutcome::Denied) | None => return login_redirect(),
    };

    // Authentication succeeded — REFUND this attempt on the shared per-IP
    // limiter, exactly as the password login does. Clearing it would wipe every
    // other account's failures from the same IP, letting one working OAuth
    // account mask a password brute-force from behind it.
    state.ip_login_limiter.refund_ip(&client);

    finish_callback(state, request.collection, verified, mfa).await
}

/// GET/POST `/admin/auth/callback/{name}` — un-scoped auth callback dispatch.
///
/// The hook module `auth_callback.{name}` is called with a context whose
/// `headers` table holds every request header plus the reserved keys
/// `_query_{param}` (each URL query parameter), `_form_{field}` (each field
/// of a urlencoded form body — a `form_post` response) and `_method` (`"GET"`
/// or `"POST"`); `ctx.collection` names the bound auth collection.
///
/// Returns a user document table (with `id` field) to create a session,
/// or `nil`/`false` to redirect to login.
///
/// This route binds to the SINGLE auth collection when there is exactly one.
/// With 2+ auth collections the target is ambiguous, so it fails closed — use
/// the collection-scoped route `/admin/auth/callback/{collection}/{name}`.
pub async fn auth_callback(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(collection) = sole_auth_collection(&state.infra.registry) else {
        warn!(
            "un-scoped auth callback {name:?} has no unambiguous auth collection \
             (need exactly one); use /admin/auth/callback/{{collection}}/{name} \
             for multi-auth-collection setups"
        );
        return login_redirect();
    };

    let form = form_fields(&headers, &body);
    let request =
        CallbackRequest::builder(addr, &collection, &name, &method, &params, &form, &headers)
            .build();

    complete_auth_callback(&state, &request).await
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use axum::http::HeaderValue;

    use super::*;
    use crate::core::{CollectionDefinition, FieldDefinition, FieldType};

    /// The hook sees every header plus each query parameter as `_query_{k}` —
    /// a query parameter can never pose as a header.
    #[test]
    fn the_hook_context_carries_headers_and_prefixed_query_params() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-user", HeaderValue::from_static("alice"));
        let params = HashMap::from([
            ("code".to_string(), "abc".to_string()),
            ("x-forwarded-user".to_string(), "mallory".to_string()),
        ]);
        let form = HashMap::new();
        let addr = SocketAddr::from(([127, 0, 0, 1], 80));
        let request =
            CallbackRequest::builder(addr, "users", "sso", &Method::GET, &params, &form, &headers)
                .build();

        let ctx = hook_context(&request);

        assert_eq!(
            ctx.get("x-forwarded-user").map(String::as_str),
            Some("alice")
        );
        assert_eq!(ctx.get("_query_code").map(String::as_str), Some("abc"));
        assert_eq!(
            ctx.get("_query_x-forwarded-user").map(String::as_str),
            Some("mallory")
        );
        assert_eq!(ctx.get("_method").map(String::as_str), Some("GET"));
        assert_eq!(ctx.len(), 4);
    }

    /// Regression: a `form_post` callback's body never reached the hook, so
    /// the `code` / `state` an identity provider posted were lost. The form
    /// fields arrive as `_form_{k}`, next to the method.
    #[test]
    fn the_hook_context_carries_form_post_fields() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        let body = Bytes::from_static(b"code=abc&state=s%201");
        let form = form_fields(&headers, &body);
        let params = HashMap::new();
        let addr = SocketAddr::from(([127, 0, 0, 1], 80));
        let request = CallbackRequest::builder(
            addr,
            "users",
            "sso",
            &Method::POST,
            &params,
            &form,
            &headers,
        )
        .build();

        let ctx = hook_context(&request);

        assert_eq!(ctx.get("_form_code").map(String::as_str), Some("abc"));
        assert_eq!(ctx.get("_form_state").map(String::as_str), Some("s 1"));
        assert_eq!(ctx.get("_method").map(String::as_str), Some("POST"));
    }

    /// A body that is not a urlencoded form contributes no fields.
    #[test]
    fn a_non_form_body_has_no_fields() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/json"));

        assert!(form_fields(&headers, &Bytes::from_static(b"code=abc")).is_empty());
    }

    /// A request header spelled like a framework key is dropped: `_query_*`,
    /// `_form_*` and `_method` only ever come from the query, the body and
    /// the request line.
    #[test]
    fn a_header_cannot_pose_as_a_framework_key() {
        let mut headers = HeaderMap::new();
        headers.insert("_query_state", HeaderValue::from_static("forged"));
        headers.insert("_form_code", HeaderValue::from_static("forged"));
        headers.insert("_method", HeaderValue::from_static("PUT"));
        let (params, form) = (HashMap::new(), HashMap::new());
        let addr = SocketAddr::from(([127, 0, 0, 1], 80));
        let request =
            CallbackRequest::builder(addr, "users", "sso", &Method::GET, &params, &form, &headers)
                .build();

        let ctx = hook_context(&request);

        assert!(!ctx.contains_key("_query_state"));
        assert!(!ctx.contains_key("_form_code"));
        assert_eq!(ctx.get("_method").map(String::as_str), Some("GET"));
    }

    /// A soft-deleting `users` auth collection holding `u1`.
    fn users() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                email TEXT,
                _locked INTEGER DEFAULT 0,
                _verified INTEGER DEFAULT 1,
                _session_version INTEGER DEFAULT 3,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, email) VALUES ('u1', 'stored@x.com');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::new(true));
        def.soft_delete = true;
        def.fields = vec![FieldDefinition::builder("email", FieldType::Email).build()];

        (conn, def)
    }

    fn hook_doc() -> Document {
        let mut doc = Document::builder("u1").build();
        doc.fields
            .insert("email".to_string(), json!("claimed@evil.com"));

        doc
    }

    /// Regression: the callback gated and minted from the hook's table — the
    /// session's email came from whatever the hook returned. It now works
    /// from the stored document.
    #[test]
    fn the_session_is_built_from_the_stored_user() {
        let (conn, def) = users();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let (user, session_version) = callback_admission(&ctx, &hook_doc()).expect("admitted");

        assert_eq!(user.get_str("email"), Some("stored@x.com"));
        assert_eq!(session_version, 3);
    }

    /// Regression: the callback checked existence with a lookup that counts
    /// trashed rows, so a soft-deleted user got a session.
    #[test]
    fn a_trashed_user_gets_no_session() {
        let (conn, def) = users();
        conn.execute(
            "UPDATE users SET _deleted_at = '2026-01-01T00:00:00Z' WHERE id = 'u1'",
            [],
        )
        .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        assert!(callback_admission(&ctx, &hook_doc()).is_none());
    }

    /// A locked user gets no session.
    #[test]
    fn a_locked_user_gets_no_session() {
        let (conn, def) = users();
        conn.execute("UPDATE users SET _locked = 1 WHERE id = 'u1'", [])
            .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        assert!(callback_admission(&ctx, &hook_doc()).is_none());
    }
}
