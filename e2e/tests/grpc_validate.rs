//! gRPC e2e: `Validate` RPC.
//!
//! Validate exercises the same validation pipeline as Create but
//! doesn't persist. Real clients use it to check forms before
//! submission. The in-process trait tests cover the validation
//! semantics; this file pins the wire framing of the response's
//! `map<string, string> errors`.

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

use crap_cms::{
    api::content::{ValidateRequest, content_api_client::ContentApiClient},
    core::{collection::*, field::*},
};
use crap_cms_e2e::spawn_grpc_server;

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Text).build(),
    ];
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

// ── validate_valid_data_returns_valid_true ───────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn validate_valid_data_returns_valid_true() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let resp = client
        .validate(ValidateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "OK"), ("body", "anything")])),
            ..Default::default()
        })
        .await
        .expect("validate")
        .into_inner();

    assert!(resp.valid, "valid data should report valid=true");
    assert!(
        resp.errors.is_empty(),
        "errors map should be empty on valid"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── validate_missing_required_returns_errors_map ─────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn validate_missing_required_returns_errors_map() {
    let ctx = spawn_grpc_server(vec![make_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let resp = client
        .validate(ValidateRequest {
            collection: "posts".to_string(),
            data: Some(DataMap {
                fields: HashMap::new(),
            }),
            ..Default::default()
        })
        .await
        .expect("validate")
        .into_inner();

    assert!(
        !resp.valid,
        "missing required field should report valid=false"
    );
    assert!(
        resp.errors.contains_key("title"),
        "errors map should contain 'title' key, got: {:?}",
        resp.errors
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
