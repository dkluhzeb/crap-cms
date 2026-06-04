//! gRPC e2e: the unified auth evaluator honors per-method
//! `surfaces` and (for strategies) `activates_on` over the wire.
//!
//! Each test pins one aspect of the new evaluator contract:
//!
//! 1. **`default_methods_collection_login_and_find_work`** — baseline:
//!    a collection with the standard `password_login + bearer +
//!    session_cookie` set behaves identically to `auth = { enabled =
//!    true }`. Both Login and anonymous Find succeed.
//! 2. **`omitted_password_login_rejects_login_rpc`** — Login refuses
//!    when the target collection's `methods` list omits
//!    `password_login`.
//! 3. **`api_key_strategy_authenticates_when_header_present`** — a
//!    `strategy` method with `activates_on = { header = "x-api-key"
//!    }` and `surfaces = ["grpc"]` is invoked over gRPC when the
//!    header is on the request, and the resulting principal passes
//!    a read-access check that would deny anonymous callers.
//! 4. **`api_key_strategy_skipped_without_header`** — same setup,
//!    but the request omits the header → the strategy is not
//!    invoked and the read-access check denies the (anonymous)
//!    request.
//! 5. **`admin_only_strategy_does_not_fire_over_grpc`** — a strategy
//!    declared with `surfaces = ["admin"]` is not invoked for a
//!    gRPC request, even with the activating header present.
//! 6. **`bearer_admin_only_jwt_not_accepted_over_grpc`** — a JWT
//!    issued by the Login RPC for a collection whose `bearer`
//!    method is `surfaces = ["admin"]` is not accepted as a
//!    principal on subsequent gRPC calls.
//! 7. **`strategy_returning_locked_user_is_refused`** — pins the
//!    fix for a security bug: a strategy hook that returns a doc
//!    with `_locked = 1` must NOT authenticate the user. Bearer /
//!    cookie paths reject locked users via `resolve_token`; the
//!    strategy path now mirrors the check.
//! 8. **`unaccepted_bearer_returns_unauthenticated`** — a valid
//!    bearer whose collection no longer accepts bearer on this
//!    surface AND no strategy matches now returns
//!    `Unauthenticated` (was silently `Anonymous` → cookie loop
//!    on admin / blanket-allow on gRPC).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::used_underscore_binding,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal
)]

use std::collections::BTreeMap;

use prost_types::{Struct, Value, value::Kind};
use tonic::{Code, Request, metadata::MetadataValue};

use crap_cms::{
    api::content::{
        CreateRequest, FindRequest, LoginRequest, content_api_client::ContentApiClient,
    },
    core::{
        Access,
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::{spawn_grpc_server, spawn_grpc_server_with_lua};

// ── Lua fixtures ─────────────────────────────────────────────────────────

/// Strategy hook: returns the first user in the collection
/// whenever invoked. The activation discriminator (header
/// presence) is what gates calling the hook — the hook itself
/// just confirms the strategy can produce a principal.
const ANY_USER_STRATEGY: &str = r"
local M = {}
function M.authenticate(ctx)
    local result = crap.collections.find(ctx.collection, { limit = 1 })
    if not result or not result.documents or #result.documents == 0 then
        return nil
    end
    return result.documents[1]
end
return M
";

/// Read-access function that allows authenticated callers only.
/// Used by the strategy-firing tests to distinguish anonymous
/// (denied) from authenticated (allowed).
const DENY_ANONYMOUS_READ: &str = r"
local M = {}
function M.read(ctx)
    if ctx.user == nil then
        return false
    end
    return true
end
return M
";

/// Strategy hook that always returns a synthesized, locked user
/// document. Used to verify the evaluator's strategy path refuses
/// `_locked = 1` users (mirrors the bearer/cookie locked check).
const RETURNS_LOCKED_USER_STRATEGY: &str = r#"
local M = {}
function M.authenticate(_ctx)
    return {
        id = "locked-u1",
        email = "locked@x.com",
        _locked = 1,
    }
end
return M
"#;

// ── helpers ──────────────────────────────────────────────────────────────

fn proto_struct(pairs: &[(&str, &str)]) -> Struct {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(
            (*k).to_string(),
            Value {
                kind: Some(Kind::StringValue((*v).to_string())),
            },
        );
    }
    Struct { fields }
}

fn base_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("User".to_string())),
        plural: Some(LocalizedString::Plain("Users".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("name", FieldType::Text).build(),
    ];
    def
}

/// Plain non-auth collection used by the strategy/surfaces tests
/// as the resource whose read is gated by `deny_anonymous`. The
/// strategy lives on `users`; this isolates the auth side from the
/// resource side so the strategy's internal `crap.collections.find`
/// (against `users`) isn't itself blocked by the test gate.
fn posts_def_anonymous_read_denied() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def.access = Access {
        read: Some("hooks.deny_anonymous.read".to_string()),
        ..Default::default()
    };
    def
}

fn users_def_password_only() -> CollectionDefinition {
    let mut def = base_users_def();
    def.auth = Some(Auth::enabled());
    def
}

/// Collection that intentionally omits `password_login` from its
/// methods — only `bearer` is listed.
fn users_def_no_password_login() -> CollectionDefinition {
    let mut def = base_users_def();
    def.auth = Some(Auth {
        enabled: true,
        token_expiry: 7200,
        methods: vec![AuthMethod::Bearer {
            surfaces: SurfaceSet::all(),
        }],
    });
    def
}

/// `users` collection with default methods + a header-activated
/// strategy scoped to gRPC. The strategy authenticates by looking
/// up the first user via `crap.collections.find` (no access fn on
/// `users`, so the internal lookup isn't blocked).
fn users_def_with_api_key_strategy() -> CollectionDefinition {
    let mut def = base_users_def();
    let mut auth = Auth::enabled();
    auth.methods.push(AuthMethod::strategy_on_header(
        "api-key",
        "hooks.api_key.authenticate",
        "x-api-key",
        SurfaceSet::grpc_only(),
    ));
    def.auth = Some(auth);
    def
}

/// Same as above but the strategy is declared with `surfaces =
/// ["admin"]`, so it should never fire over gRPC.
fn users_def_with_admin_only_strategy() -> CollectionDefinition {
    let mut def = base_users_def();
    let mut auth = Auth::enabled();
    auth.methods.push(AuthMethod::Strategy {
        name: "admin-only".to_string(),
        authenticate: "hooks.api_key.authenticate".to_string(),
        activates_on: Activation::Header {
            header: "x-api-key".to_string(),
        },
        surfaces: SurfaceSet::admin_only(),
    });
    def.auth = Some(auth);
    def
}

/// Default methods but bearer is scoped to admin only — a JWT
/// issued by Login will be ignored on subsequent gRPC requests.
fn users_def_bearer_admin_only() -> CollectionDefinition {
    let mut def = base_users_def();
    let auth = Auth {
        enabled: true,
        token_expiry: 7200,
        methods: vec![
            AuthMethod::password_login(),
            AuthMethod::Bearer {
                surfaces: SurfaceSet::admin_only(),
            },
        ],
    };
    def.auth = Some(auth);
    def
}

/// Attach an `x-api-key` header to a tonic request.
fn with_api_key<T>(mut req: Request<T>, value: &str) -> Request<T> {
    let v: MetadataValue<_> = value.parse().expect("static api-key value parses");
    req.metadata_mut().insert("x-api-key", v);
    req
}

/// Attach a Bearer token to a tonic request.
fn with_bearer<T>(mut req: Request<T>, token: &str) -> Request<T> {
    let v: MetadataValue<_> = format!("Bearer {token}")
        .parse()
        .expect("bearer header parses");
    req.metadata_mut().insert("authorization", v);
    req
}

// ── baseline: default methods ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn default_methods_collection_login_and_find_work() {
    let ctx = spawn_grpc_server(vec![users_def_password_only()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "a@x.com"),
                ("name", "Alice"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create");

    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "a@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("login should work with default methods")
        .into_inner()
        .token;
    assert!(!token.is_empty());

    client
        .find(FindRequest {
            collection: "users".to_string(),
            ..Default::default()
        })
        .await
        .expect("anon find on collection without read-access fn should work");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── omitted password_login → Login refused ───────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn omitted_password_login_rejects_login_rpc() {
    let ctx = spawn_grpc_server(vec![users_def_no_password_login()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "anyone@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect_err("Login on a no-password-login collection must fail");
    assert_ne!(
        status.code(),
        Code::Ok,
        "Login should not succeed when password_login is omitted from methods"
    );
    assert_ne!(
        status.code(),
        Code::Internal,
        "should be a user-recoverable error, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── strategy: header activation honored over grpc ────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn api_key_strategy_authenticates_when_header_present() {
    let ctx = spawn_grpc_server_with_lua(
        vec![
            users_def_with_api_key_strategy(),
            posts_def_anonymous_read_denied(),
        ],
        vec![],
        &[
            ("hooks/api_key.lua", ANY_USER_STRATEGY),
            ("hooks/deny_anonymous.lua", DENY_ANONYMOUS_READ),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // Seed a user the strategy can authenticate as.
    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "key@x.com"),
                ("name", "Key User"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");

    // With the activation header → strategy fires (on users) →
    // user is set on the request → posts read-access gate allows.
    let req = with_api_key(
        Request::new(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }),
        "some-key-value",
    );
    client
        .find(req)
        .await
        .expect("find posts with x-api-key should be allowed by deny_anonymous gate");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_strategy_skipped_without_header() {
    let ctx = spawn_grpc_server_with_lua(
        vec![
            users_def_with_api_key_strategy(),
            posts_def_anonymous_read_denied(),
        ],
        vec![],
        &[
            ("hooks/api_key.lua", ANY_USER_STRATEGY),
            ("hooks/deny_anonymous.lua", DENY_ANONYMOUS_READ),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "key@x.com"),
                ("name", "Key User"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create");

    // No activation header → strategy must NOT fire → caller is
    // anonymous → deny_anonymous denies the read.
    let status = client
        .find(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect_err("anonymous find should be denied by access fn");
    assert_eq!(
        status.code(),
        Code::PermissionDenied,
        "expected PermissionDenied, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── strategy: surfaces filter honored ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn admin_only_strategy_does_not_fire_over_grpc() {
    let ctx = spawn_grpc_server_with_lua(
        vec![
            users_def_with_admin_only_strategy(),
            posts_def_anonymous_read_denied(),
        ],
        vec![],
        &[
            ("hooks/api_key.lua", ANY_USER_STRATEGY),
            ("hooks/deny_anonymous.lua", DENY_ANONYMOUS_READ),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "key@x.com"),
                ("name", "Key User"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create");

    // Header is present but the strategy is scoped to surfaces=[admin],
    // so a gRPC request must not invoke it. Caller stays anonymous →
    // deny_anonymous denies.
    let req = with_api_key(
        Request::new(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }),
        "doesnt-matter",
    );
    let status = client
        .find(req)
        .await
        .expect_err("admin-only strategy must not authenticate over grpc");
    assert_eq!(
        status.code(),
        Code::PermissionDenied,
        "expected PermissionDenied (strategy stayed quiet), got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── bearer: surfaces filter rejects out-of-surface JWTs ──────────────────

#[tokio::test(flavor = "multi_thread")]
async fn bearer_admin_only_jwt_not_accepted_over_grpc() {
    let ctx = spawn_grpc_server_with_lua(
        vec![
            users_def_bearer_admin_only(),
            posts_def_anonymous_read_denied(),
        ],
        vec![],
        &[("hooks/deny_anonymous.lua", DENY_ANONYMOUS_READ)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "a@x.com"),
                ("name", "Alice"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create");

    // Login still issues a JWT — password_login is allowed; only the
    // *acceptance* of that JWT on the gRPC surface is gated.
    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "a@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("login itself should succeed")
        .into_inner()
        .token;
    assert!(!token.is_empty());

    // The JWT is presented on gRPC but bearer.surfaces excludes grpc.
    // Evaluator now returns Invalid(Unaccepted) → Unauthenticated
    // (was previously silently Anonymous, which let the cookie path
    // loop on admin and made gRPC clients keep retrying).
    let req = with_bearer(
        Request::new(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }),
        &token,
    );
    let status = client
        .find(req)
        .await
        .expect_err("bearer surfaces=[admin] must not authenticate over grpc");
    assert_eq!(
        status.code(),
        Code::Unauthenticated,
        "expected Unauthenticated (bearer dead for this surface), got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── strategy: locked-user refusal ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn strategy_returning_locked_user_is_refused() {
    let ctx = spawn_grpc_server_with_lua(
        vec![
            users_def_with_api_key_strategy(),
            posts_def_anonymous_read_denied(),
        ],
        vec![],
        &[
            ("hooks/api_key.lua", RETURNS_LOCKED_USER_STRATEGY),
            ("hooks/deny_anonymous.lua", DENY_ANONYMOUS_READ),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // Strategy fires (header present, surfaces match) but returns a
    // locked user. Evaluator must refuse → caller stays anonymous →
    // deny_anonymous denies.
    let req = with_api_key(
        Request::new(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }),
        "doesnt-matter",
    );
    let status = client
        .find(req)
        .await
        .expect_err("locked user must not authenticate via strategy");
    assert_eq!(
        status.code(),
        Code::PermissionDenied,
        "expected PermissionDenied (locked user refused), got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── credential supplied but unaccepted → Unauthenticated ───────────

#[tokio::test(flavor = "multi_thread")]
async fn unaccepted_bearer_returns_unauthenticated() {
    // Same as `bearer_admin_only_jwt_not_accepted_over_grpc` but
    // calls a route WITHOUT an access fn — proves the `Unaccepted`
    // failure is surfaced even when the resource would normally
    // permit anonymous reads.
    let ctx = spawn_grpc_server(vec![users_def_bearer_admin_only()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "a@x.com"),
                ("name", "Alice"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create");

    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "a@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("login")
        .into_inner()
        .token;

    // No access fn on users — anonymous Find would normally succeed.
    // But because a bearer WAS supplied and resolves to a collection
    // that doesn't accept bearer on grpc, the evaluator now returns
    // Invalid(Unaccepted) → 401 instead of silently treating the
    // request as anonymous.
    let req = with_bearer(
        Request::new(FindRequest {
            collection: "users".to_string(),
            ..Default::default()
        }),
        &token,
    );
    let status = client
        .find(req)
        .await
        .expect_err("supplied-but-unaccepted bearer must surface as 401");
    assert_eq!(
        status.code(),
        Code::Unauthenticated,
        "expected Unauthenticated, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
