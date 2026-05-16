//! gRPC e2e: server-streaming `Subscribe` over real TCP.
//!
//! The Subscribe RPC is server-streaming — the in-process trait
//! tests in `tests/grpc_subscribe_jobs.rs` exercise the broadcast
//! plumbing, but they call the trait method directly so the actual
//! streaming framing never crosses the network. This test verifies
//! that an event published by a Create RPC reaches a client whose
//! `Subscribe` stream is held open over a real `tonic::Channel`.

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
use tokio::time::timeout;
use tokio_stream::StreamExt;

use crap_cms::{
    api::content::{CreateRequest, SubscribeRequest, content_api_client::ContentApiClient},
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

// ── subscribe_streams_create_event_over_wire ─────────────────────────────
//
// Open a Subscribe stream, then issue a Create from a second client.
// The event should arrive on the streaming connection within a few
// seconds. Both clients share the same `Channel` — tonic multiplexes
// the two RPCs over the same HTTP/2 connection.

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_streams_create_event_over_wire() {
    let ctx = spawn_grpc_server(vec![make_posts_def()], vec![]).await;

    let mut subscriber = ContentApiClient::new(ctx.channel.clone());
    let mut writer = ContentApiClient::new(ctx.channel.clone());

    let mut stream = subscriber
        .subscribe(SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        })
        .await
        .expect("open subscribe stream")
        .into_inner();

    writer
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "Streamed Post")])),
            ..Default::default()
        })
        .await
        .expect("create post");

    let event = timeout(Duration::from_secs(3), stream.next())
        .await
        .expect("event should arrive within 3s")
        .expect("stream should not end")
        .expect("event should be ok");

    assert_eq!(event.collection, "posts", "event collection should match");
    assert_eq!(event.operation, "create", "event op should be create");
    assert!(
        !event.document_id.is_empty(),
        "event should carry the created doc id"
    );

    // Drop the stream so the server-side subscriber task notices the
    // client went away, then let the runtime drop the rest. Awaiting
    // server_handle would block on the open subscribe stream draining.
    drop(stream);
    ctx.shutdown.cancel();
}
