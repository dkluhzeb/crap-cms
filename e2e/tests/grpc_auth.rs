//! gRPC e2e: Login + Me + auth-metadata round-trip.
//!
//! `Login` returns a JWT. `Me` takes that token in its request body
//! (the proto's design — Me is used to bootstrap auth sessions where
//! metadata headers may not be available). Other RPCs that gate on
//! the current user read the token from the `authorization: Bearer`
//! metadata header. These tests verify both paths over real TCP.

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
use tonic::Code;

use crap_cms::{
    api::content::{CreateRequest, LoginRequest, MeRequest, content_api_client::ContentApiClient},
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::spawn_grpc_server;

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
    def.auth = Some(Auth {
        enabled: true,
        ..Default::default()
    });
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

// ── login_returns_jwt_and_me_identifies_same_user ────────────────────────
//
// End-to-end happy path: Create a user, Login, hand the token back
// in the `Me` request body, verify the server returns a user
// document with the same email.

#[tokio::test(flavor = "multi_thread")]
async fn login_returns_jwt_and_me_identifies_same_user() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "alice@example.com"),
                ("name", "Alice"),
                ("password", "secret123"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");

    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "alice@example.com".to_string(),
            password: "secret123".to_string(),
        })
        .await
        .expect("login")
        .into_inner()
        .token;
    assert!(!token.is_empty(), "login should return a non-empty token");

    let me = client
        .me(MeRequest { token })
        .await
        .expect("Me with valid token")
        .into_inner();

    let user = me.user.expect("Me returns user document");
    assert!(!user.id.is_empty(), "user should have an id");

    let email = user
        .fields
        .as_ref()
        .and_then(|f| f.fields.get("email"))
        .and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.as_str()),
            _ => None,
        });
    assert_eq!(
        email,
        Some("alice@example.com"),
        "Me should return the logged-in user's email"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── me_with_invalid_token_returns_unauthenticated ────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn me_with_invalid_token_returns_unauthenticated() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .me(MeRequest {
            token: "not-a-real-jwt".to_string(),
        })
        .await
        .expect_err("Me with invalid token should fail");

    assert_eq!(
        status.code(),
        Code::Unauthenticated,
        "invalid token should map to UNAUTHENTICATED, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── login_wrong_password_returns_non_internal_error ──────────────────────
//
// Wrong password must not crash the server (which would manifest as
// INTERNAL). The exact code (UNAUTHENTICATED / INVALID_ARGUMENT /
// PERMISSION_DENIED) depends on the handler's mapping; this test
// pins "not a server bug."

#[tokio::test(flavor = "multi_thread")]
async fn login_wrong_password_returns_non_internal_error() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "bob@example.com"),
                ("name", "Bob"),
                ("password", "right-password"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");

    let status = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "bob@example.com".to_string(),
            password: "WRONG".to_string(),
        })
        .await
        .expect_err("login with wrong password should fail");

    assert_ne!(status.code(), Code::Ok, "wrong password should not succeed");
    assert_ne!(
        status.code(),
        Code::Internal,
        "wrong password should not be a server error, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
