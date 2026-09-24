//! Email-verification integration tests for admin HTTP handlers.
//!
//! Covers: verify-email tokens, login gating on unverified accounts, and the
//! verify-email rate limiter.

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
    core::{DocumentFields, rate_limit::IP_VERIFY_EMAIL_KEYSPACE},
    db::query::{self, TokenGrant},
};

use admin_auth_support::{
    TEST_CSRF, body_string, csrf_cookie, make_users_def, make_verify_users_def, setup_app,
};

// ── Email Verification Tests ──────────────────────────────────────────────

#[tokio::test]
async fn verify_email_invalid_token() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/verify-email?token=badtoken&collection=users")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Expected 200 or redirect, got {status}"
    );
}

#[tokio::test]
async fn login_unverified_email() {
    let app = setup_app(vec![make_verify_users_def()], vec![]);

    let def = {
        let reg = &app.registry;
        reg.get_collection("vusers").unwrap().clone()
    };

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("email".to_string(), json!("unverified@test.com")),
        ("name".to_string(), json!("Unverified User")),
    ])
    .into();
    let doc = query::create(&tx, "vusers", &def, &data, None).unwrap();
    query::update_password(&tx, "vusers", &doc.id, "secret123").unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/login")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::from(
                    "collection=vusers&email=unverified@test.com&password=secret123",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Unverified login should fail gracefully, got {status}"
    );
    if status == StatusCode::OK {
        let body = body_string(resp.into_body()).await;
        assert!(
            body.to_lowercase().contains("verify") || body.to_lowercase().contains("error"),
            "Login page should show verification error"
        );
    }
}

#[tokio::test]
async fn verify_email_with_valid_token() {
    let app = setup_app(vec![make_verify_users_def()], vec![]);

    let def = {
        let reg = &app.registry;
        reg.get_collection("vusers").unwrap().clone()
    };

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("email".to_string(), json!("toverify@test.com")),
        ("name".to_string(), json!("To Verify")),
    ])
    .into();
    let doc = query::create(&tx, "vusers", &def, &data, None).unwrap();
    query::update_password(&tx, "vusers", &doc.id, "secret123").unwrap();
    tx.commit().unwrap();

    let token = "valid-verification-token-12345";
    {
        let conn = app.pool.get().unwrap();
        query::set_verification_token(
            &conn,
            &TokenGrant::builder("vusers", &doc.id, token, 9999999999).build(),
        )
        .unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/verify-email?token={token}"))
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Successful verification should redirect, got {status}"
    );
    if let Some(location) = resp.headers().get("location") {
        let loc = location.to_str().unwrap_or("");
        assert!(
            loc.contains("login") && loc.contains("success"),
            "Should redirect to login with success message, got {loc}"
        );
    }
}

/// Regression: the verify-email handler counts every attempt against its OWN
/// per-IP keyspace (`IP_VERIFY_EMAIL_KEYSPACE`, derived from the forgot-password
/// IP limiter with `rescoped`) via the atomic `check_and_block` — deliberately
/// NOT the forgot-password counter itself, so a burst of verification attempts
/// can't exhaust a legitimate reset's budget. Seeding that scoped limiter to one below
/// the threshold and making a single verify request must tip it over. Blocked
/// and invalid-token both redirect to login, so the limiter state — not the HTTP
/// response — is the observable proof.
#[tokio::test]
async fn verify_email_attempt_counts_against_ip_limiter() {
    let app = setup_app(vec![make_verify_users_def()], vec![]);
    let ip = "127.0.0.1"; // ConnectInfo below + trust_proxy=off → this key

    // Derive the handler's limiter exactly as it does: same backend and
    // thresholds as the forgot-password IP limiter, its own keyspace.
    let verify_limiter = app
        .ip_forgot_password_limiter
        .rescoped(IP_VERIFY_EMAIL_KEYSPACE);

    // Seed to 19 so one more trips it.
    for _ in 0..19 {
        let _ = verify_limiter.check_and_block(ip);
    }
    assert!(
        !verify_limiter.is_blocked(ip),
        "precondition: not yet blocked at 19/20"
    );

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/verify-email?token=wrong-token")
                .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(resp.status(), StatusCode::SEE_OTHER | StatusCode::FOUND),
        "verify-email redirects regardless of outcome, got {}",
        resp.status()
    );
    assert!(
        verify_limiter.is_blocked(ip),
        "the verify attempt must have advanced the scoped IP limiter to its threshold"
    );
}
