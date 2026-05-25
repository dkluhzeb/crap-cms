//! gRPC e2e: bulk CRUD (`CreateMany` / `UpdateMany` / `DeleteMany`).
//!
//! Wire-level coverage for the bulk operations. The in-process trait
//! tests verify the batch semantics, but only the wire-level test
//! catches a future regression where `repeated google.protobuf.Struct`
//! framing breaks for large batches.

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

use crap_cms::{
    api::content::{
        CountRequest, CreateManyRequest, DeleteManyRequest, UpdateManyRequest,
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
        FieldDefinition::builder("status", FieldType::Text).build(),
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

// ── create_many_inserts_all_documents ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn create_many_inserts_all_documents() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let docs: Vec<Struct> = (0..7)
        .map(|i| proto_struct(&[("title", &format!("Bulk {i}")), ("status", "draft")]))
        .collect();

    let resp = client
        .create_many(CreateManyRequest {
            collection: "posts".to_string(),
            documents: docs,
            ..Default::default()
        })
        .await
        .expect("create_many")
        .into_inner();
    assert_eq!(resp.created, 7, "should report 7 created");

    let count = client
        .count(CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("count")
        .into_inner()
        .count;
    assert_eq!(count, 7, "count should match created");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── update_many_applies_to_all_matching ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn update_many_applies_to_all_matching() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let docs: Vec<Struct> = (0..5)
        .map(|i| proto_struct(&[("title", &format!("Doc {i}")), ("status", "draft")]))
        .collect();
    client
        .create_many(CreateManyRequest {
            collection: "posts".to_string(),
            documents: docs,
            ..Default::default()
        })
        .await
        .expect("create_many");

    // Update all to status = published.
    let resp = client
        .update_many(UpdateManyRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("status", "published")])),
            ..Default::default()
        })
        .await
        .expect("update_many")
        .into_inner();
    assert_eq!(resp.modified, 5, "should report 5 modified");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── delete_many_removes_all_matching ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn delete_many_removes_all_matching() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let docs: Vec<Struct> = (0..4)
        .map(|i| proto_struct(&[("title", &format!("Trash {i}"))]))
        .collect();
    client
        .create_many(CreateManyRequest {
            collection: "posts".to_string(),
            documents: docs,
            ..Default::default()
        })
        .await
        .expect("create_many");

    let resp = client
        .delete_many(DeleteManyRequest {
            collection: "posts".to_string(),
            force_hard_delete: true,
            ..Default::default()
        })
        .await
        .expect("delete_many")
        .into_inner();
    assert_eq!(resp.deleted, 4, "should report 4 deleted");

    let count = client
        .count(CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("count")
        .into_inner()
        .count;
    assert_eq!(count, 0, "collection should be empty after delete_many");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
