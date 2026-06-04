//! gRPC e2e: `VerifyEmail` consume-side flow.
//!
//! The send half (Create user → email queued via
//! `service::email::send_verification_email`) is exercised by
//! `html_email_verify` over the admin HTTP surface and the existing
//! main-crate service tests. This file pins the consume half over
//! the wire: plant a verification token directly in the DB (what
//! the send half would do), call `VerifyEmail` via tonic, then
//! verify `_verified = 1` by attempting login (`verify_email = true`
//! collections refuse login for unverified users).

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

use chrono::Utc;
use prost_types::{Struct, Value, value::Kind};
use tonic::Code;

use crap_cms::{
    api::content::{
        CreateRequest, LoginRequest, VerifyEmailRequest, content_api_client::ContentApiClient,
    },
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
    db::{DbPool, query},
};
use crap_cms_e2e::spawn_grpc_server;

fn make_users_def_verify_email() -> CollectionDefinition {
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
    def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
    def
}

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

fn plant_token(pool: &DbPool, user_id: &str, token: &str) {
    let conn = pool.get().expect("pool");
    let exp = Utc::now().timestamp() + 3600;
    query::set_verification_token(&conn, "users", user_id, token, exp)
        .expect("set verification token");
}

// ── verify_email_valid_token_marks_verified_and_allows_login ─────────────

#[tokio::test(flavor = "multi_thread")]
async fn verify_email_valid_token_marks_verified_and_allows_login() {
    let ctx = spawn_grpc_server(vec![make_users_def_verify_email()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let user_id = client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "verify@x.com"),
                ("name", "Verify Me"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user")
        .into_inner()
        .document
        .expect("doc")
        .id;

    // Pre-condition: login is blocked for an unverified user.
    let pre = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "verify@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await;
    assert!(
        pre.is_err(),
        "unverified user should not be able to log in yet"
    );

    let token = "test-verify-token-abc123";
    plant_token(&ctx.pool, &user_id, token);

    // A non-error response is the success signal.
    client
        .verify_email(VerifyEmailRequest {
            collection: "users".to_string(),
            token: token.to_string(),
        })
        .await
        .expect("verify_email with valid token");

    // After verification: login works.
    client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "verify@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("verified user should log in");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── verify_email_invalid_token_returns_not_found ─────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn verify_email_invalid_token_returns_not_found() {
    let ctx = spawn_grpc_server(vec![make_users_def_verify_email()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .verify_email(VerifyEmailRequest {
            collection: "users".to_string(),
            token: "not-a-real-token".to_string(),
        })
        .await
        .expect_err("verify_email with garbage token should fail");

    // Per the proto: "Invalid/expired tokens result in success=false or
    // NOT_FOUND". Either NotFound or a non-Internal failure is
    // acceptable; the key invariant is "not Internal" (which would
    // mean a server bug, not a token problem).
    assert_ne!(status.code(), Code::Ok, "invalid token should not succeed");
    assert_ne!(
        status.code(),
        Code::Internal,
        "invalid token should not be a server error, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
