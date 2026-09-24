//! Auth-callback and MFA integration tests for admin HTTP handlers.
//!
//! Covers: the un-scoped and collection-scoped auth callbacks (verification,
//! cross-collection binding, MFA step) and MFA verification rate limiting.

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

mod admin_auth_support;

use serde_json::json;
use std::{collections::HashMap, net::SocketAddr};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crap_cms::{
    config::CrapConfig,
    core::{
        DocumentFields, HookRef, auth,
        collection::{Auth, CollectionDefinition, MfaMode},
        field::{FieldDefinition, FieldType},
    },
    db::query,
};

use admin_auth_support::{
    TEST_CSRF, TestApp, csrf_cookie, make_named_auth_def, make_users_def, make_verify_users_def,
    setup_app, setup_app_in_dir,
};

// ── Auth Callback Tests ───────────────────────────────────────────────────

/// Regression: an auth-callback (OAuth/external strategy) must NOT mint a
/// session for an unverified user when the collection requires email
/// verification. The per-request session resolver re-checks lock + session
/// version but NOT verification, so without the callback's own guard an
/// external strategy would bypass the verify-email requirement that the
/// password-login path enforces.
#[tokio::test]
async fn auth_callback_rejects_unverified_user() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // The callback hook ref is `auth_callback.{name}`, resolved via
    // `require("auth_callback.test")` against `{config_dir}/?.lua`, and the
    // resolver's fast path expects the module to BE the function.
    let cb_dir = tmp.path().join("auth_callback");
    std::fs::create_dir_all(&cb_dir).unwrap();
    std::fs::write(
        cb_dir.join("test.lua"),
        r#"
-- Simulate a provider that authenticated the user named by the query. The
-- handler re-reads lock/verify/session state from the DB by id, so returning
-- the id (plus email for the session claim) is enough to exercise the gating.
return function(ctx)
    local uid = ctx.headers["_query_uid"]
    if not uid then return nil end
    return { id = uid, email = "oauth@test.com" }
end
"#,
    )
    .unwrap();

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(vec![make_verify_users_def()], vec![], config, tmp);

    // An unverified user in the verify-email collection.
    let user_id = {
        let def = app.registry.get_collection("vusers").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("email".to_string(), json!("oauth@test.com"))]).into();
        let doc = query::create(&tx, "vusers", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    let session_issued = |resp: &axum::response::Response| {
        resp.headers()
            .get_all("set-cookie")
            .iter()
            .any(|v| v.to_str().unwrap_or("").contains("crap_session="))
    };

    // Unverified → the callback must not issue a session (redirect to login).
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/auth/callback/test?uid={user_id}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !session_issued(&resp),
        "unverified user must not receive a session via the auth callback"
    );

    // Verify the user, retry → a session is issued.
    {
        let conn = app.pool.get().unwrap();
        query::mark_verified(&conn, "vusers", &user_id).unwrap();
    }

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/auth/callback/test?uid={user_id}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        session_issued(&resp),
        "verified user should receive a session via the auth callback"
    );
}

/// Security: the un-scoped callback `/admin/auth/callback/{name}` fails closed
/// when the target collection is ambiguous. With 2+ auth collections (`acol`,
/// `bcol`) it can't know which to bind, so it never mints a session at all —
/// operators must use the collection-scoped route. (Previously it bound to the
/// lexicographically-first collection; the silent min-binding was a footgun.)
/// Here a user that exists only in `bcol` gets no session via the un-scoped route.
#[tokio::test]
async fn auth_callback_does_not_bind_across_collections() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cb_dir = tmp.path().join("auth_callback");
    std::fs::create_dir_all(&cb_dir).unwrap();
    std::fs::write(
        cb_dir.join("test.lua"),
        r#"
return function(ctx)
    local uid = ctx.headers["_query_uid"]
    if not uid then return nil end
    return { id = uid, email = "x@test.com" }
end
"#,
    )
    .unwrap();

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(
        vec![make_named_auth_def("acol"), make_named_auth_def("bcol")],
        vec![],
        config,
        tmp,
    );

    // The user exists only in `bcol`, but the callback runs under `acol` (min).
    let user_id = {
        let def = app.registry.get_collection("bcol").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("email".to_string(), json!("x@test.com"))]).into();
        let doc = query::create(&tx, "bcol", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/auth/callback/test?uid={user_id}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let has_session = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|c| c.starts_with("crap_session="));
    assert!(
        !has_session,
        "a user that exists only in another auth collection must not get a session"
    );
}

/// Helper for the scoped-callback tests: write the shared `test` callback hook
/// (echoes the `uid` query param as the user id) into the config dir.
fn write_uid_callback_hook(tmp_path: &std::path::Path) {
    let cb_dir = tmp_path.join("auth_callback");
    std::fs::create_dir_all(&cb_dir).unwrap();
    std::fs::write(
        cb_dir.join("test.lua"),
        r#"
return function(ctx)
    local uid = ctx.headers["_query_uid"]
    if not uid then return nil end
    return { id = uid, email = "x@test.com" }
end
"#,
    )
    .unwrap();
}

fn issued_session(resp: &axum::response::Response) -> bool {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|c| c.starts_with("crap_session="))
}

/// The collection-scoped route `/admin/auth/callback/{collection}/{name}` binds
/// the session to the collection named in the URL — so OAuth works for a
/// NON-first auth collection. With `acol` + `bcol`, a `bcol` user logging in via
/// `/admin/auth/callback/bcol/test` gets a session (the un-scoped route can't).
#[tokio::test]
async fn auth_callback_scoped_binds_to_named_collection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_uid_callback_hook(tmp.path());

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(
        vec![make_named_auth_def("acol"), make_named_auth_def("bcol")],
        vec![],
        config,
        tmp,
    );

    let user_id = {
        let def = app.registry.get_collection("bcol").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("email".to_string(), json!("x@test.com"))]).into();
        let doc = query::create(&tx, "bcol", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/auth/callback/bcol/test?uid={user_id}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        issued_session(&resp),
        "scoped callback to bcol must mint a session for a bcol user"
    );
}

/// Security: the scoped route preserves the no-cross-collection-binding
/// guarantee. A `bcol` user routed through `/admin/auth/callback/acol/test` is
/// refused — `validate_callback_user` requires the user to exist in the named
/// (`acol`) collection, so a hook-returned id from another collection can't mint
/// a session there.
#[tokio::test]
async fn auth_callback_scoped_does_not_bind_across_collections() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_uid_callback_hook(tmp.path());

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(
        vec![make_named_auth_def("acol"), make_named_auth_def("bcol")],
        vec![],
        config,
        tmp,
    );

    let user_id = {
        let def = app.registry.get_collection("bcol").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("email".to_string(), json!("x@test.com"))]).into();
        let doc = query::create(&tx, "bcol", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    // Bind attempt against acol with a bcol-only user id.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/auth/callback/acol/test?uid={user_id}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        !issued_session(&resp),
        "scoped callback to acol must refuse a user that exists only in bcol"
    );
}

/// The scoped route fails closed for an unknown / non-auth collection in the URL.
#[tokio::test]
async fn auth_callback_scoped_rejects_unknown_collection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_uid_callback_hook(tmp.path());

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(vec![make_named_auth_def("acol")], vec![], config, tmp);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/auth/callback/nope/test?uid=whatever")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        !issued_session(&resp),
        "scoped callback to an unknown collection must not mint a session"
    );
}

/// An app whose `musers` collection requires custom-delivered MFA (the
/// `mfa_deliver` hook writes each code into `outbox`) and exempts the
/// `exempt` callbacks from it, plus one `musers` user (its id is returned).
fn mfa_callback_app(exempt: &[&str]) -> (TestApp, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_uid_callback_hook(tmp.path());

    let hook_dir = tmp.path().join("mfa_hooks");
    std::fs::create_dir_all(&hook_dir).unwrap();
    std::fs::write(
        hook_dir.join("deliver.lua"),
        r#"
return function(ctx)
    crap.collections.create("outbox", { body = ctx.code },
        { override_access = true, hooks = false, events = false })
end
"#,
    )
    .unwrap();

    let exempt: Vec<String> = exempt.iter().map(|s| (*s).to_string()).collect();
    let mut users = make_named_auth_def("musers");
    users.auth = Some(Auth::enabled().map_password_login(|b| {
        b.mfa(MfaMode::Custom)
            .mfa_deliver(Some(HookRef::new("mfa_hooks.deliver")))
            .mfa_exempt_callbacks(exempt)
    }));

    let mut outbox = CollectionDefinition::new("outbox");
    outbox.fields = vec![FieldDefinition::builder("body", FieldType::Text).build()];

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(vec![users, outbox], vec![], config, tmp);

    let user_id = {
        let def = app.registry.get_collection("musers").unwrap().clone();
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("email".to_string(), json!("mfa@test.com"))]).into();
        let doc = query::create(&tx, "musers", &def, &data, None).unwrap();
        tx.commit().unwrap();
        doc.id.to_string()
    };

    (app, user_id)
}

/// `GET` a callback route from a fixed peer address.
async fn get_callback(app: &TestApp, uri: &str) -> axum::response::Response {
    app.router
        .clone()
        .oneshot(
            Request::get(uri)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// The `crap_mfa_pending` cookie value a response set, if any.
fn mfa_pending_token(resp: &axum::response::Response) -> Option<String> {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|c| c.strip_prefix("crap_mfa_pending="))
        .and_then(|rest| rest.split(';').next())
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

/// The code the `mfa_deliver` hook wrote into `outbox`. Delivery runs in a
/// background task, so poll briefly.
async fn delivered_code(app: &TestApp) -> String {
    let def = app.registry.get_collection("outbox").unwrap().clone();

    for _ in 0..50 {
        let docs = {
            let conn = app.pool.get().unwrap();
            query::find(&conn, "outbox", &def, &query::FindQuery::default(), None).unwrap()
        };

        if let Some(code) = docs.first().and_then(|d| d.get_str("body")) {
            return code.to_string();
        }

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    panic!("the mfa_deliver hook never delivered a code");
}

/// Regression: an auth callback minted a session without the collection's
/// MFA step, so an OAuth login skipped the second factor a password login
/// must complete. The callback now yields the pending-MFA step (no session);
/// completing it mints the session. A callback the collection exempts by
/// *another* name does not exempt this one.
#[tokio::test]
async fn auth_callback_on_an_mfa_collection_requires_the_second_factor() {
    let (app, user_id) = mfa_callback_app(&["okta"]);

    let resp = get_callback(
        &app,
        &format!("/admin/auth/callback/musers/test?uid={user_id}"),
    )
    .await;

    assert!(
        !issued_session(&resp),
        "the callback must not mint a session before the second factor"
    );
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert_eq!(location, "/admin/mfa?collection=musers");
    let pending = mfa_pending_token(&resp).expect("the MFA pending cookie is set");

    let code = delivered_code(&app).await;
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/mfa")
                .header("content-type", "application/x-www-form-urlencoded")
                .header(
                    "Cookie",
                    format!("{}; crap_mfa_pending={pending}", csrf_cookie()),
                )
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(format!("code={code}")))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        issued_session(&resp),
        "completing the MFA step mints the callback's session"
    );
}

/// A callback the collection lists in `mfa_exempt_callbacks` (its identity
/// provider enforces the second factor) mints the session directly.
#[tokio::test]
async fn an_mfa_exempt_auth_callback_mints_the_session_directly() {
    let (app, user_id) = mfa_callback_app(&["test"]);

    let resp = get_callback(
        &app,
        &format!("/admin/auth/callback/musers/test?uid={user_id}"),
    )
    .await;

    assert!(
        issued_session(&resp),
        "an exempt callback skips the MFA step"
    );
    assert!(mfa_pending_token(&resp).is_none());
}

// ── MFA Rate Limiting ─────────────────────────────────────────────────────

/// Regression: MFA code verification must be rate-limited. Each `POST /admin/mfa`
/// attempt counts against BOTH the per-user and per-IP MFA limiters via the
/// atomic `check_and_block`, recorded up front before the code is checked.
/// Seeding both to one below their thresholds and making a single attempt must
/// tip both over — proving the gate is wired in both dimensions. Before this
/// fix the endpoint had no limiter at all, so a 6-digit code was brute-forceable
/// within the pending window.
#[tokio::test]
async fn mfa_verification_attempt_counts_against_limiters() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = "mfa-user-1";
    let ip = "127.0.0.1"; // ConnectInfo below + trust_proxy=off → this key

    // A valid MFA pending token, signed the same way the login challenge step
    // signs it — `token_use = MfaPending` is required, since `verify_mfa_action`
    // rejects a plain session token (the MFA-bypass fix) before reaching the
    // limiter gate. The user need not exist — the gate runs before the code is
    // verified, which is exactly the behavior under test.
    let claims = auth::Claims::builder(user_id, "users")
        .email("mfa@test.com")
        .token_use(auth::TokenUse::MfaPending)
        .exp((chrono::Utc::now().timestamp() as u64) + 300)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();

    // Harness MFA limiters: per-user 5/window, per-IP 20/window. Seed each to
    // one below threshold so a single attempt tips both.
    for _ in 0..4 {
        let _ = app.mfa_limiter.check_and_block(user_id);
    }
    for _ in 0..19 {
        let _ = app.ip_mfa_limiter.check_and_block(ip);
    }
    assert!(
        !app.mfa_limiter.is_blocked(user_id) && !app.ip_mfa_limiter.is_blocked(ip),
        "precondition: neither limiter blocked yet"
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/mfa")
                .header("content-type", "application/x-www-form-urlencoded")
                .header(
                    "Cookie",
                    format!("{}; crap_mfa_pending={token}", csrf_cookie()),
                )
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from("code=000000"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the MFA form re-renders regardless of outcome"
    );
    assert!(
        app.mfa_limiter.is_blocked(user_id),
        "the MFA attempt must advance the per-user MFA limiter to its threshold"
    );
    assert!(
        app.ip_mfa_limiter.is_blocked(ip),
        "the MFA attempt must advance the per-IP MFA limiter to its threshold"
    );
}
