//! gRPC e2e: server reflection.
//!
//! Verifies `grpc.reflection.v1.ServerReflection/ServerReflectionInfo`
//! lists the registered `ContentAPI` service. Reflection is what
//! `grpcurl -plaintext localhost:50051 list` consumes; if this test
//! regresses, ad-hoc gRPC debugging without local proto files
//! stops working.

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

use crap_cms::core::{collection::*, field::LocalizedString};
use crap_cms_e2e::spawn_grpc_server;
use tokio_stream::StreamExt;
use tonic::Request;
use tonic_reflection::pb::v1::{
    ServerReflectionRequest, server_reflection_client::ServerReflectionClient,
    server_reflection_request::MessageRequest, server_reflection_response::MessageResponse,
};

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def
}

// ── reflection_lists_content_api_service ─────────────────────────────────
//
// `list_services` returns every registered gRPC service. We expect to
// see `crap.ContentAPI` plus the two infrastructure services
// (`grpc.health.v1.Health` and `grpc.reflection.v1.ServerReflection`).

#[tokio::test(flavor = "multi_thread")]
async fn reflection_lists_content_api_service() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;

    let mut client = ServerReflectionClient::new(ctx.channel.clone());

    let req_stream = tokio_stream::iter(vec![ServerReflectionRequest {
        host: String::new(),
        message_request: Some(MessageRequest::ListServices(String::new())),
    }]);

    let mut resp = client
        .server_reflection_info(Request::new(req_stream))
        .await
        .expect("ServerReflectionInfo should succeed")
        .into_inner();

    let msg = resp
        .next()
        .await
        .expect("reflection should send a response")
        .expect("response should be ok");

    let services = match msg.message_response {
        Some(MessageResponse::ListServicesResponse(list)) => list.service,
        other => panic!("expected ListServicesResponse, got {other:?}"),
    };

    let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"crap.ContentAPI"),
        "reflection should list crap.ContentAPI, got: {names:?}"
    );
    // `grpc.health.v1.Health` is intentionally NOT in the reflection
    // list — `tonic-health` doesn't register its descriptor unless
    // `tonic_reflection::server::Builder::register_health_service()`
    // is called, which crap-cms's server doesn't do. We only register
    // the application proto. Verify that reflection itself is
    // present (sanity check for the reflection wiring).
    assert!(
        names.contains(&"grpc.reflection.v1.ServerReflection"),
        "reflection should self-register, got: {names:?}"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
