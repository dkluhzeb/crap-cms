//! gRPC e2e: full CRUD round-trip over the wire.
//!
//! Pairs nicely with `grpc_smoke` (which just verifies Find on an
//! empty collection). This file exercises every CRUD verb in one
//! sequence — Create → `FindByID` → Update → Find (verifies the update
//! landed) → Delete → Undelete — plus standalone `Count` happy
//! paths. The in-process trait tests cover the same operations, but
//! never via a real tonic channel, so things like the `optional`
//! prost-encoding of partial-update fields aren't exercised at the
//! wire level until now.

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
        CountRequest, CreateRequest, DeleteRequest, FindByIdRequest, FindRequest, UndeleteRequest,
        UpdateRequest, content_api_client::ContentApiClient,
    },
    core::{collection::*, field::*},
};
use crap_cms_e2e::spawn_grpc_server;

fn make_soft_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.soft_delete = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Text).build(),
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

fn get_string(doc: &crap_cms::api::content::Document, field: &str) -> Option<String> {
    doc.fields.as_ref().and_then(|s| {
        s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    })
}

// ── create_find_update_delete_undelete_full_round_trip ───────────────────

#[tokio::test(flavor = "multi_thread")]
async fn create_find_update_delete_undelete_full_round_trip() {
    let ctx = spawn_grpc_server(vec![make_soft_posts_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // CREATE
    let created = client
        .create(CreateRequest {
            events: None,
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "Original"), ("body", "v1")])),
            ..Default::default()
        })
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("created doc");

    let id = created.id.clone();
    assert!(!id.is_empty(), "created doc should have an id");

    // FIND_BY_ID
    let fetched = client
        .find_by_id(FindByIdRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            ..Default::default()
        })
        .await
        .expect("find_by_id")
        .into_inner()
        .document
        .expect("found doc");
    assert_eq!(get_string(&fetched, "title").as_deref(), Some("Original"));

    // UPDATE — partial: only title; body should remain unchanged.
    client
        .update(UpdateRequest {
            events: None,
            collection: "posts".to_string(),
            id: id.clone(),
            data: Some(proto_struct(&[("title", "Updated")])),
            ..Default::default()
        })
        .await
        .expect("update");

    // FIND — verify update landed and body was preserved.
    let after_update = client
        .find_by_id(FindByIdRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            ..Default::default()
        })
        .await
        .expect("find_by_id after update")
        .into_inner()
        .document
        .expect("doc still exists");
    assert_eq!(
        get_string(&after_update, "title").as_deref(),
        Some("Updated")
    );
    assert_eq!(
        get_string(&after_update, "body").as_deref(),
        Some("v1"),
        "partial update should not clear body"
    );

    // DELETE — soft-delete (collection has soft_delete = true).
    let del = client
        .delete(DeleteRequest {
            events: None,
            collection: "posts".to_string(),
            id: id.clone(),
            force_hard_delete: false,
        })
        .await
        .expect("delete")
        .into_inner();
    assert!(
        del.soft_deleted,
        "soft-delete collection should soft-delete"
    );

    // FIND — soft-deleted doc no longer in default list.
    let active = client
        .find(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("find")
        .into_inner();
    assert_eq!(
        active.documents.len(),
        0,
        "soft-deleted doc should be hidden from default find"
    );

    // UNDELETE — restore the soft-deleted doc.
    client
        .undelete(UndeleteRequest {
            collection: "posts".to_string(),
            id: id.clone(),
        })
        .await
        .expect("undelete");

    // FIND — doc is back.
    let restored = client
        .find(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("find after undelete")
        .into_inner();
    assert_eq!(restored.documents.len(), 1);
    assert_eq!(restored.documents[0].id, id);

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── count_reflects_collection_size ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn count_reflects_collection_size() {
    let ctx = spawn_grpc_server(vec![make_soft_posts_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let initial = client
        .count(CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("count empty")
        .into_inner()
        .count;
    assert_eq!(initial, 0, "empty collection counts 0");

    for i in 0..5 {
        client
            .create(CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[("title", &format!("Post {i}"))])),
                ..Default::default()
            })
            .await
            .expect("create");
    }

    let after = client
        .count(CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("count after creates")
        .into_inner()
        .count;
    assert_eq!(after, 5, "should count 5 docs after 5 creates");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── delete_with_force_hard_delete_removes_permanently ────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn delete_with_force_hard_delete_removes_permanently() {
    let ctx = spawn_grpc_server(vec![make_soft_posts_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let id = client
        .create(CreateRequest {
            events: None,
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "Doomed")])),
            ..Default::default()
        })
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("doc")
        .id;

    let del = client
        .delete(DeleteRequest {
            events: None,
            collection: "posts".to_string(),
            id: id.clone(),
            force_hard_delete: true,
        })
        .await
        .expect("hard delete")
        .into_inner();
    assert!(
        !del.soft_deleted,
        "force_hard_delete should bypass soft-delete"
    );

    // Undelete should fail — doc is gone.
    let undel = client
        .undelete(UndeleteRequest {
            collection: "posts".to_string(),
            id,
        })
        .await;
    assert!(
        undel.is_err(),
        "undelete of hard-deleted doc should fail, got: {:?}",
        undel.map(tonic::Response::into_inner)
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
