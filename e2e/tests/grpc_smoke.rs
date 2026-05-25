//! Smoke test for the gRPC e2e harness.
//!
//! Proves the full stack works end-to-end over real TCP:
//! `spawn_grpc_server` binds a tonic server, the test connects a
//! `tonic` channel, and the `Find` RPC round-trips. If this fails,
//! every other gRPC e2e test will too — fix this first.

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

use crap_cms::{
    api::content::{FindRequest, content_api_client::ContentApiClient},
    core::{collection::*, field::*},
};
use crap_cms_e2e::spawn_grpc_server;

fn make_posts_def() -> CollectionDefinition {
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

// ── find_on_empty_collection_returns_zero_documents ──────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn find_on_empty_collection_returns_zero_documents() {
    let ctx = spawn_grpc_server(vec![make_posts_def()], vec![]).await;

    let mut client = ContentApiClient::new(ctx.channel.clone());

    let resp = client
        .find(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("Find over the wire should succeed");

    let body = resp.into_inner();
    assert_eq!(body.documents.len(), 0, "empty collection returns no docs");
    assert_eq!(
        body.pagination
            .as_ref()
            .expect("pagination present")
            .total_docs,
        0,
        "total_docs should be 0 for empty collection"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
