//! gRPC e2e: full forgot-password → reset → login flow.
//!
//! Security-sensitive path. Real production deployments rely on
//! every link in this chain working over the wire:
//! `ForgotPassword` queues an email, the user clicks the link with
//! the token, `ResetPassword` accepts the token + new password, and
//! subsequent `Login` calls succeed with the new password (and fail
//! with the old one). The in-process trait tests cover each step
//! individually; this file exercises the full flow over a real
//! `tonic::Channel`.

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
use std::time::Duration;

use prost_types::{Struct, Value, value::Kind};
use tonic::Code;

use crap_cms::{
    api::content::{
        CreateRequest, ForgotPasswordRequest, LoginRequest, ResetPasswordRequest,
        content_api_client::ContentApiClient,
    },
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::{extract_token, spawn_grpc_server, wait_for_queued_email_in_pool};

fn make_users_def() -> CollectionDefinition {
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
    def.auth = Some(Auth::enabled());
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

// ── forgot_password_reset_login_full_flow ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn forgot_password_reset_login_full_flow() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "carol@example.com"),
                ("name", "Carol"),
                ("password", "old-pass-123"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");

    // Verify old password actually works before reset.
    client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "carol@example.com".to_string(),
            password: "old-pass-123".to_string(),
        })
        .await
        .expect("login with old password before reset");

    // Trigger password reset — queues an email.
    client
        .forgot_password(ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "carol@example.com".to_string(),
        })
        .await
        .expect("forgot_password");

    let pool = ctx.pool.clone();
    let email = tokio::task::spawn_blocking(move || {
        wait_for_queued_email_in_pool(&pool, "carol@example.com", Duration::from_secs(2))
    })
    .await
    .expect("spawn_blocking")
    .expect("reset email should be queued within 2s");

    let token = extract_token(&email, "/admin/reset-password")
        .expect("reset email should carry a /admin/reset-password?token=...");

    client
        .reset_password(ResetPasswordRequest {
            collection: "users".to_string(),
            token,
            new_password: "new-pass-456".to_string(),
        })
        .await
        .expect("reset_password");

    // New password works.
    client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "carol@example.com".to_string(),
            password: "new-pass-456".to_string(),
        })
        .await
        .expect("login with new password after reset");

    // Old password no longer works.
    let old_login = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "carol@example.com".to_string(),
            password: "old-pass-123".to_string(),
        })
        .await;
    assert!(
        old_login.is_err(),
        "old password should be rejected after reset"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── forgot_password_succeeds_for_unknown_email ───────────────────────────
//
// Per the proto: ForgotPassword always returns success (true) even
// when the email doesn't exist — leaking which emails are registered
// is a known anti-pattern. Verifies this over the wire.

#[tokio::test(flavor = "multi_thread")]
async fn forgot_password_succeeds_for_unknown_email() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // A non-error response is the success signal — an unknown email must not
    // leak its absence by erroring (anti-enumeration).
    client
        .forgot_password(ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "nobody@example.com".to_string(),
        })
        .await
        .expect("forgot_password for unknown email should not error");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── reset_password_with_invalid_token_returns_error ──────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn reset_password_with_invalid_token_returns_error() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .reset_password(ResetPasswordRequest {
            collection: "users".to_string(),
            token: "not-a-real-token".to_string(),
            new_password: "whatever".to_string(),
        })
        .await
        .expect_err("reset_password with garbage token should fail");

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
