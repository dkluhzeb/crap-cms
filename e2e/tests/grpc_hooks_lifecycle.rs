//! gRPC e2e: lifecycle hooks fire correctly over the wire.
//!
//! Lua hooks bound to a collection's lifecycle (`before_validate`,
//! `before_change`, `before_read`, `after_read`) and field-level
//! `before_change` are exercised by main-crate trait tests; this
//! file pins that they survive a real `tonic::Channel` round-trip
//! — incoming `data` mutations land in storage, outgoing field
//! transformations reach the client, and synthetic errors map to
//! the right gRPC status.

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
        CreateRequest, FindByIdRequest, FindRequest, content_api_client::ContentApiClient,
    },
    core::{
        collection::*,
        field::{FieldDefinition, FieldHooks, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::spawn_grpc_server_with_lua;

// ── Lua fixtures ─────────────────────────────────────────────────────────

/// Field-level `before_change`: derives a slug from `name` when the
/// field arrives empty. Returns the new value for the field.
const SLUG_GEN: &str = r#"
local M = {}
function M.auto_slug(value, ctx)
    if (value == nil or value == "") and ctx.data and ctx.data.name then
        local s = ctx.data.name:lower()
        s = s:gsub("[^%w%s-]", "")
        s = s:gsub("%s+", "-")
        return s
    end
    return value
end
return M
"#;

/// Collection-level `before_change`: stamps `ctx.data.stamp` so we
/// can observe the hook's mutation in storage after the round-trip.
const STAMP_HOOK: &str = r"
local M = {}
function M.stamp_input(ctx)
    if ctx.data then
        ctx.data.stamp = 'stamped'
    end
    return ctx
end
return M
";

/// Collection-level `before_validate`: rejects titles containing
/// the literal word `FORBIDDEN`.
const MODERATOR: &str = r#"
local M = {}
function M.reject_forbidden(ctx)
    if ctx.data and ctx.data.title and ctx.data.title:find("FORBIDDEN") then
        error("Title contains forbidden word")
    end
    return ctx
end
return M
"#;

/// Collection-level `after_read`: adds a computed `computed`
/// field by concatenating "read:" with the title. We verify it
/// reaches the client over the wire.
const COMPUTED_READ: &str = r#"
local M = {}
function M.add_computed(ctx)
    if ctx.data and ctx.data.title then
        ctx.data.computed = "read:" .. ctx.data.title
    end
    return ctx
end
return M
"#;

/// Collection-level `before_read`: strips the `internal` field
/// before it reaches the read pipeline. (`before_read` runs
/// against the query, not a single doc; we use it as a soft check
/// — if the field appears in the response, the hook chain ran
/// without errors but didn't strip — which is fine for this test's
/// purpose of "hook fires without crashing the chain.")
const NOOP_BEFORE_READ: &str = r"
local M = {}
function M.touch(ctx)
    return ctx
end
return M
";

// ── helpers ──────────────────────────────────────────────────────────────

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

fn get_str(doc: &crap_cms::api::content::Document, field: &str) -> Option<String> {
    doc.fields.as_ref().and_then(|s| {
        s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    })
}

fn posts_def_with(hooks: Hooks, extra_fields: Vec<FieldDefinition>) -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.hooks = hooks;
    let mut fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    fields.extend(extra_fields);
    def.fields = fields;
    def
}

// ── field_level_before_change_derives_slug_from_name ─────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn field_level_before_change_derives_slug_from_name() {
    let mut name_f = FieldDefinition::builder("name", FieldType::Text)
        .required(true)
        .build();
    name_f.hooks = FieldHooks::default();

    let mut slug_f = FieldDefinition::builder("slug", FieldType::Text).build();
    slug_f.hooks = FieldHooks {
        before_change: vec!["hooks.slug_gen.auto_slug".to_string()],
        ..Default::default()
    };

    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        name_f,
        slug_f,
    ];

    let ctx =
        spawn_grpc_server_with_lua(vec![def], vec![], &[("hooks/slug_gen.lua", SLUG_GEN)]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let doc = client
        .create(CreateRequest {
            collection: "articles".to_string(),
            data: Some(proto_struct(&[("name", "Hello World")])),
            ..Default::default()
        })
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("doc");

    // The before_change hook should have derived slug="hello-world"
    // from name="Hello World" because slug was empty on input.
    assert_eq!(
        get_str(&doc, "slug").as_deref(),
        Some("hello-world"),
        "field-level before_change should have derived the slug from name"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── collection_before_change_mutation_persists ───────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn collection_before_change_mutation_persists() {
    let def = posts_def_with(
        Hooks {
            before_change: vec!["hooks.stamp.stamp_input".to_string()],
            ..Default::default()
        },
        vec![FieldDefinition::builder("stamp", FieldType::Text).build()],
    );

    let ctx =
        spawn_grpc_server_with_lua(vec![def], vec![], &[("hooks/stamp.lua", STAMP_HOOK)]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let post_id = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "Hello")])),
            ..Default::default()
        })
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("doc")
        .id;

    // FindByID round-trips back the post; if the before_change hook
    // mutated ctx.data.stamp, it should now be stored.
    let after = client
        .find_by_id(FindByIdRequest {
            collection: "posts".to_string(),
            id: post_id,
            ..Default::default()
        })
        .await
        .expect("find_by_id")
        .into_inner()
        .document
        .expect("doc");

    assert_eq!(
        get_str(&after, "stamp").as_deref(),
        Some("stamped"),
        "before_change should have stamped the data; find_by_id should see it"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── before_validate_error_maps_to_invalid_argument ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn before_validate_error_maps_to_invalid_argument() {
    let def = posts_def_with(
        Hooks {
            before_validate: vec!["hooks.moderator.reject_forbidden".to_string()],
            ..Default::default()
        },
        vec![],
    );

    let ctx =
        spawn_grpc_server_with_lua(vec![def], vec![], &[("hooks/moderator.lua", MODERATOR)]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // Allowed title.
    client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "OK")])),
            ..Default::default()
        })
        .await
        .expect("non-forbidden create should succeed");

    // Forbidden title.
    let status = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "FORBIDDEN word here")])),
            ..Default::default()
        })
        .await
        .expect_err("forbidden create should fail");
    assert_eq!(
        status.code(),
        tonic::Code::InvalidArgument,
        "before_validate error → INVALID_ARGUMENT, got {:?}: {}",
        status.code(),
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── after_read_adds_computed_field_visible_on_wire ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn after_read_adds_computed_field_visible_on_wire() {
    // Define `computed` as an actual field on the collection so the
    // hook's added value survives DocumentFields filtering.
    let def = posts_def_with(
        Hooks {
            after_read: vec!["hooks.computed.add_computed".to_string()],
            ..Default::default()
        },
        vec![FieldDefinition::builder("computed", FieldType::Text).build()],
    );

    let ctx =
        spawn_grpc_server_with_lua(vec![def], vec![], &[("hooks/computed.lua", COMPUTED_READ)])
            .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let post_id = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "Headline")])),
            ..Default::default()
        })
        .await
        .expect("create")
        .into_inner()
        .document
        .expect("doc")
        .id;

    let after = client
        .find_by_id(FindByIdRequest {
            collection: "posts".to_string(),
            id: post_id,
            ..Default::default()
        })
        .await
        .expect("find_by_id")
        .into_inner()
        .document
        .expect("doc");

    assert_eq!(
        get_str(&after, "computed").as_deref(),
        Some("read:Headline"),
        "after_read should have added the computed field; client should see it over the wire"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── before_read_runs_without_breaking_chain ──────────────────────────────
//
// `before_read` hooks operate on the query stage, not on a single
// document, so the easiest behavioural test is just "registering one
// doesn't break Find". A real strip-via-before_read pattern would
// inspect filters and ALTER them — testing that is beyond this
// file's transport-level scope (covered by main-crate hook tests).

#[tokio::test(flavor = "multi_thread")]
async fn before_read_hook_runs_without_breaking_find() {
    let def = posts_def_with(
        Hooks {
            before_read: vec!["hooks.noop_read.touch".to_string()],
            ..Default::default()
        },
        vec![],
    );

    let ctx = spawn_grpc_server_with_lua(
        vec![def],
        vec![],
        &[("hooks/noop_read.lua", NOOP_BEFORE_READ)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "A")])),
            ..Default::default()
        })
        .await
        .expect("create");

    let found = client
        .find(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect("find with before_read hook should not error")
        .into_inner();
    assert_eq!(found.documents.len(), 1);

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
