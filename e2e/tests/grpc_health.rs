//! gRPC e2e: health-check service.
//!
//! Verifies the standard `grpc.health.v1.Health/Check` RPC works over
//! the wire and reports `SERVING` for the registered `ContentAPI`
//! service. The health service is wired up by `spawn_grpc_server`
//! mirroring `api::server::start`; this test would catch a regression
//! where someone drops `add_service(health_service)` from the layer
//! chain.

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
use tonic_health::ServingStatus;
use tonic_health::pb::{HealthCheckRequest, health_client::HealthClient};

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def
}

// ── health_check_empty_service_returns_serving ───────────────────────────
//
// An empty `service` field asks for the overall server health (as
// opposed to a specific service). tonic-health returns `SERVING` here.

#[tokio::test(flavor = "multi_thread")]
async fn health_check_empty_service_returns_serving() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;

    let mut client = HealthClient::new(ctx.channel.clone());

    let resp = client
        .check(HealthCheckRequest {
            service: String::new(),
        })
        .await
        .expect("Health/Check should succeed");

    assert_eq!(
        resp.into_inner().status,
        ServingStatus::Serving as i32,
        "overall server should report SERVING"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── health_check_content_service_returns_serving ─────────────────────────
//
// `spawn_grpc_server` registers the `ContentAPI` service as SERVING.
// The full proto-package + service name is the lookup key.

#[tokio::test(flavor = "multi_thread")]
async fn health_check_content_service_returns_serving() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;

    let mut client = HealthClient::new(ctx.channel.clone());

    let resp = client
        .check(HealthCheckRequest {
            service: "crap.ContentAPI".to_string(),
        })
        .await
        .expect("Health/Check for ContentAPI should succeed");

    assert_eq!(
        resp.into_inner().status,
        ServingStatus::Serving as i32,
        "ContentAPI should report SERVING"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
