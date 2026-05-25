//! gRPC e2e: error-code mapping over the wire.
//!
//! `From<ServiceError> for Status` is the single mapping layer
//! between crap-cms's internal error vocabulary and gRPC status
//! codes (see `src/api/handlers/collection/error_mapping.rs`). The
//! in-process trait tests verify the mapping function directly, but
//! only the wire-level test catches a regression where a status
//! code's encoding changes between server and client tonic
//! versions, or where a handler accidentally returns
//! `Status::internal` on a known error path.

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
    api::content::{
        CreateRequest, DescribeCollectionRequest, FindByIdRequest, FindRequest, UpdateRequest,
        content_api_client::ContentApiClient,
    },
    core::{collection::*, field::*},
};
use crap_cms_e2e::spawn_grpc_server;

fn make_def() -> CollectionDefinition {
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

// ── unknown_collection_slug_returns_not_found ────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn unknown_collection_slug_returns_not_found() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .find(FindRequest {
            collection: "nonexistent".to_string(),
            ..Default::default()
        })
        .await
        .expect_err("Find on unknown slug should fail");

    assert_eq!(
        status.code(),
        Code::NotFound,
        "unknown slug → NOT_FOUND, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── find_by_id_unknown_id_returns_not_found ──────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn find_by_id_unknown_id_returns_not_found() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .find_by_id(FindByIdRequest {
            collection: "posts".to_string(),
            id: "does-not-exist".to_string(),
            ..Default::default()
        })
        .await
        .expect_err("FindByID on unknown id should fail");

    assert_eq!(
        status.code(),
        Code::NotFound,
        "unknown id → NOT_FOUND, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── create_missing_required_field_returns_invalid_argument ───────────────

#[tokio::test(flavor = "multi_thread")]
async fn create_missing_required_field_returns_invalid_argument() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // posts.title is required; sending nothing should fail validation.
    let status = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(Struct {
                fields: BTreeMap::new(),
            }),
            ..Default::default()
        })
        .await
        .expect_err("Create with missing required field should fail");

    assert_eq!(
        status.code(),
        Code::InvalidArgument,
        "validation failure → INVALID_ARGUMENT, got {:?}: {}",
        status.code(),
        status.message()
    );
    assert!(
        status.message().to_lowercase().contains("title"),
        "error message should mention the offending field, got: {}",
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── update_unknown_id_returns_not_found ──────────────────────────────────
//
// Regression test for a bug surfaced by this very suite: pre-fix,
// Update on a missing id came back as `Status::internal` ("Internal
// error") because `query::update` raised an untyped
// `anyhow!("Document not found after update")` and the generic
// `From<anyhow::Error> for ServiceError` mapped it to
// `ServiceError::Internal` → `Status::internal`. Production clients
// retry on `Internal` (treating it as transient), so a stale id
// triggered a retry loop. Fixed by:
//   - `query::DocumentNotFound` typed error raised when
//     `conn.execute(UPDATE …)` reports 0 affected rows
//   - `From<anyhow::Error> for ServiceError` downcasts and maps to
//     `ServiceError::NotFound` → `Status::not_found`

#[tokio::test(flavor = "multi_thread")]
async fn update_unknown_id_returns_not_found() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .update(UpdateRequest {
            collection: "posts".to_string(),
            id: "does-not-exist".to_string(),
            data: Some(proto_struct(&[("title", "Whatever")])),
            ..Default::default()
        })
        .await
        .expect_err("Update on unknown id should fail");

    assert_eq!(
        status.code(),
        Code::NotFound,
        "update unknown id → NOT_FOUND, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── describe_collection_unknown_slug_returns_not_found ───────────────────

#[tokio::test(flavor = "multi_thread")]
async fn describe_collection_unknown_slug_returns_not_found() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .describe_collection(DescribeCollectionRequest {
            slug: "ghost".to_string(),
            is_global: false,
        })
        .await
        .expect_err("Describe on unknown slug should fail");

    assert_eq!(
        status.code(),
        Code::NotFound,
        "describe unknown slug → NOT_FOUND, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
