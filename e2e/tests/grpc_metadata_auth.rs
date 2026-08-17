//! gRPC e2e: Bearer-token authentication via `authorization` metadata.
//!
//! Most authenticated RPCs (`ListJobs`, `Count`, etc.) read the JWT
//! from the gRPC `authorization` metadata header rather than from a
//! request body field. The in-process trait tests in
//! `tests/grpc_*.rs` set metadata on `tonic::Request` objects directly,
//! but that doesn't exercise the wire-level metadata serialization —
//! HTTP/2 headers, base64 encoding for binary values, header-name
//! casing, the actual extraction in the server's `extract_token` from
//! a real `MetadataMap` populated by `tonic::transport::Server`. These
//! tests close that gap by hitting `ListJobs` over a real channel.

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

use std::collections::HashMap;

use crap_cms::api::content::{DataMap, FieldValue, field_value::Kind};
use tonic::{Code, Request, metadata::MetadataValue};

use crap_cms::{
    api::content::{
        CreateRequest, ListJobsRequest, LoginRequest, content_api_client::ContentApiClient,
    },
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
    def.auth = Some(Auth::enabled());
    def
}

fn proto_struct(pairs: &[(&str, &str)]) -> DataMap {
    let mut fields = HashMap::new();
    for (k, v) in pairs {
        fields.insert(
            (*k).to_string(),
            FieldValue {
                kind: Some(Kind::StringValue((*v).to_string())),
            },
        );
    }
    DataMap { fields }
}

// ── list_jobs_without_metadata_returns_unauthenticated ───────────────────

#[tokio::test(flavor = "multi_thread")]
async fn list_jobs_without_metadata_returns_unauthenticated() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .list_jobs(ListJobsRequest {})
        .await
        .expect_err("ListJobs without auth should fail");

    assert_eq!(
        status.code(),
        Code::Unauthenticated,
        "missing authorization metadata should map to UNAUTHENTICATED, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── list_jobs_with_bearer_metadata_succeeds ──────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn list_jobs_with_bearer_metadata_succeeds() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            events: None,
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

    let mut req = Request::new(ListJobsRequest {});
    let bearer: MetadataValue<_> = format!("Bearer {token}")
        .parse()
        .expect("valid metadata value");
    req.metadata_mut().insert("authorization", bearer);

    let resp = client
        .list_jobs(req)
        .await
        .expect("ListJobs with bearer token should succeed");

    // The test app has no jobs registered, so the response is just an
    // empty list — the success itself is the assertion. (If a future
    // setup registers test jobs, this should still pass.)
    let _ = resp.into_inner().jobs;

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── list_jobs_with_invalid_bearer_returns_unauthenticated ────────────────
//
// A malformed JWT in the metadata header should map cleanly to
// UNAUTHENTICATED, not to INTERNAL (which would indicate a parser
// crash) or PERMISSION_DENIED (which would imply the user resolved
// but was disallowed).

#[tokio::test(flavor = "multi_thread")]
async fn list_jobs_with_invalid_bearer_returns_unauthenticated() {
    let ctx = spawn_grpc_server(vec![make_users_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let mut req = Request::new(ListJobsRequest {});
    let bearer: MetadataValue<_> = "Bearer not-a-real-jwt"
        .parse()
        .expect("valid metadata value");
    req.metadata_mut().insert("authorization", bearer);

    let status = client
        .list_jobs(req)
        .await
        .expect_err("ListJobs with invalid bearer should fail");

    assert_eq!(
        status.code(),
        Code::Unauthenticated,
        "invalid bearer should map to UNAUTHENTICATED, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
