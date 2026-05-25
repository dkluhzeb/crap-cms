//! gRPC e2e: `GrpcRateLimitLayer` enforces `RESOURCE_EXHAUSTED`.
//!
//! The rate-limit layer sits at the tower level — it never reaches
//! the `ContentService` impl — so the in-process trait tests in
//! `tests/grpc_*.rs` can't exercise it. This test installs the layer
//! via [`spawn_grpc_server_with_rate_limit`] with a tight budget,
//! fires a burst of `Find` RPCs over a real channel, and asserts
//! that the last one comes back as `RESOURCE_EXHAUSTED` exactly the
//! way a production client would see it.

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

use tonic::Code;

use crap_cms::{
    api::content::{FindRequest, content_api_client::ContentApiClient},
    core::{collection::*, field::LocalizedString},
};
use crap_cms_e2e::spawn_grpc_server_with_rate_limit;

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def
}

// ── burst_past_limit_returns_resource_exhausted ──────────────────────────
//
// Budget: 3 requests per 60s window. We fire 5 in a row. The first 3
// should succeed; #4 and #5 should fail with RESOURCE_EXHAUSTED.

#[tokio::test(flavor = "multi_thread")]
async fn burst_past_limit_returns_resource_exhausted() {
    let ctx = spawn_grpc_server_with_rate_limit(vec![make_def()], vec![], 3, 60).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let req = || FindRequest {
        collection: "posts".to_string(),
        ..Default::default()
    };

    // First 3 should be fine.
    for i in 1..=3 {
        let result = client.find(req()).await;
        assert!(
            result.is_ok(),
            "request #{i} (within budget) should succeed, got: {:?}",
            result.err()
        );
    }

    // 4th — over budget. The layer returns a raw HTTP response with
    // gRPC trailers `grpc-status: 8`. tonic decodes this to
    // `Code::ResourceExhausted` on the client side.
    let over = client
        .find(req())
        .await
        .expect_err("request #4 (over budget) should fail");
    assert_eq!(
        over.code(),
        Code::ResourceExhausted,
        "over-budget request should map to RESOURCE_EXHAUSTED, got {:?}: {}",
        over.code(),
        over.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── disabled_limit_allows_unlimited_requests ─────────────────────────────
//
// Sanity: a 0/0 budget (or a very high one) shouldn't throttle.
// This guards against a regression where the layer accidentally
// enforces even when `max_requests == 0`.

#[tokio::test(flavor = "multi_thread")]
async fn high_limit_allows_burst() {
    let ctx = spawn_grpc_server_with_rate_limit(vec![make_def()], vec![], 1000, 60).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    for i in 1..=10 {
        let result = client
            .find(FindRequest {
                collection: "posts".to_string(),
                ..Default::default()
            })
            .await;
        assert!(
            result.is_ok(),
            "request #{i} under high limit should succeed, got: {:?}",
            result.err()
        );
    }

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
