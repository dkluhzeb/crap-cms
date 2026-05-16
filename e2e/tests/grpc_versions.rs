//! gRPC e2e: version listing + restore.
//!
//! Versioning surfaces `ListVersions` (paginated list of snapshots
//! for a document) and `RestoreVersion` (apply an old snapshot as
//! the new live state). Real clients use these for revision history
//! UIs. The in-process trait tests cover the snapshot semantics;
//! this file pins the wire framing for the `repeated VersionInfo`
//! list and the round-trip of restored field values.

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
        CreateRequest, FindByIdRequest, ListVersionsRequest, RestoreVersionRequest, UpdateRequest,
        content_api_client::ContentApiClient,
    },
    core::{collection::*, field::*},
};
use crap_cms_e2e::spawn_grpc_server;

fn make_versioned_def() -> CollectionDefinition {
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
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 10));
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

// ── list_versions_returns_snapshots_for_each_update ──────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn list_versions_returns_snapshots_for_each_update() {
    let ctx = spawn_grpc_server(vec![make_versioned_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let id = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "v1"), ("body", "first")])),
            ..Default::default()
        })
        .await
        .expect("create v1")
        .into_inner()
        .document
        .expect("doc")
        .id;

    client
        .update(UpdateRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            data: Some(proto_struct(&[("title", "v2"), ("body", "second")])),
            ..Default::default()
        })
        .await
        .expect("update v2");

    client
        .update(UpdateRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            data: Some(proto_struct(&[("title", "v3"), ("body", "third")])),
            ..Default::default()
        })
        .await
        .expect("update v3");

    let resp = client
        .list_versions(ListVersionsRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            limit: None,
        })
        .await
        .expect("list_versions")
        .into_inner();

    // Expect 3 versions (one per create + each update). Sorted newest-first.
    assert!(
        resp.versions.len() >= 3,
        "expected at least 3 versions, got {}",
        resp.versions.len()
    );
    assert!(
        resp.versions.first().is_some_and(|v| v.latest),
        "first version (newest) should have latest=true"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── restore_version_reverts_document_fields ──────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn restore_version_reverts_document_fields() {
    let ctx = spawn_grpc_server(vec![make_versioned_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let id = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "Original"), ("body", "v1 body")])),
            ..Default::default()
        })
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("doc")
        .id;

    client
        .update(UpdateRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            data: Some(proto_struct(&[("title", "Modified"), ("body", "v2 body")])),
            ..Default::default()
        })
        .await
        .expect("update");

    // Find the v1 snapshot (the oldest = last in newest-first order).
    let versions = client
        .list_versions(ListVersionsRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            limit: None,
        })
        .await
        .expect("list_versions")
        .into_inner()
        .versions;
    let v1 = versions
        .iter()
        .find(|v| v.version == 1)
        .expect("version 1 should exist");

    client
        .restore_version(RestoreVersionRequest {
            collection: "posts".to_string(),
            document_id: id.clone(),
            version_id: v1.id.clone(),
        })
        .await
        .expect("restore_version");

    let after = client
        .find_by_id(FindByIdRequest {
            collection: "posts".to_string(),
            id: id.clone(),
            ..Default::default()
        })
        .await
        .expect("find_by_id after restore")
        .into_inner()
        .document
        .expect("doc");

    assert_eq!(
        get_string(&after, "title").as_deref(),
        Some("Original"),
        "title should revert to v1 value"
    );
    assert_eq!(
        get_string(&after, "body").as_deref(),
        Some("v1 body"),
        "body should revert to v1 value"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
