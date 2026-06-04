//! gRPC e2e: globals over the wire (`GetGlobal` / `UpdateGlobal`).
//!
//! The existing in-process trait tests cover global semantics, but
//! the e2e crate has zero coverage of the globals surface. This file
//! closes that gap with a get-update-get round-trip.

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
    api::content::{GetGlobalRequest, UpdateGlobalRequest, content_api_client::ContentApiClient},
    core::{collection::*, field::*},
};
use crap_cms_e2e::spawn_grpc_server;

fn make_settings_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![
        FieldDefinition::builder("site_name", FieldType::Text).build(),
        FieldDefinition::builder("tagline", FieldType::Text).build(),
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

// ── get_global_auto_creates_on_first_access ──────────────────────────────
//
// Globals are auto-created on first access — `GetGlobal` should
// return a (possibly empty) document for a registered slug even if
// no `UpdateGlobal` has run yet.

#[tokio::test(flavor = "multi_thread")]
async fn get_global_auto_creates_on_first_access() {
    let ctx = spawn_grpc_server(vec![], vec![make_settings_def()]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let resp = client
        .get_global(GetGlobalRequest {
            slug: "settings".to_string(),
            ..Default::default()
        })
        .await
        .expect("get_global on fresh global")
        .into_inner();

    let doc = resp.document.expect("global doc always present");
    assert!(!doc.id.is_empty(), "auto-created global has an id");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── update_global_round_trips_through_get ────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn update_global_round_trips_through_get() {
    let ctx = spawn_grpc_server(vec![], vec![make_settings_def()]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .update_global(UpdateGlobalRequest {
            events: None,
            slug: "settings".to_string(),
            data: Some(proto_struct(&[
                ("site_name", "Crap CMS"),
                ("tagline", "Just enough CMS"),
            ])),
            ..Default::default()
        })
        .await
        .expect("update_global");

    let doc = client
        .get_global(GetGlobalRequest {
            slug: "settings".to_string(),
            ..Default::default()
        })
        .await
        .expect("get_global after update")
        .into_inner()
        .document
        .expect("doc present");

    assert_eq!(
        get_string(&doc, "site_name").as_deref(),
        Some("Crap CMS"),
        "site_name should round-trip"
    );
    assert_eq!(
        get_string(&doc, "tagline").as_deref(),
        Some("Just enough CMS"),
        "tagline should round-trip"
    );

    // Partial update: only site_name; tagline should remain.
    client
        .update_global(UpdateGlobalRequest {
            events: None,
            slug: "settings".to_string(),
            data: Some(proto_struct(&[("site_name", "New Name")])),
            ..Default::default()
        })
        .await
        .expect("partial update_global");

    let after = client
        .get_global(GetGlobalRequest {
            slug: "settings".to_string(),
            ..Default::default()
        })
        .await
        .expect("get_global after partial update")
        .into_inner()
        .document
        .expect("doc present");
    assert_eq!(get_string(&after, "site_name").as_deref(), Some("New Name"));
    assert_eq!(
        get_string(&after, "tagline").as_deref(),
        Some("Just enough CMS"),
        "partial update should not clear tagline"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
