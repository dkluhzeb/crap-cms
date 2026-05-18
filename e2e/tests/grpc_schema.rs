//! gRPC e2e: schema introspection (`ListCollections` / `DescribeCollection`).
//!
//! Real clients (JS/TS SDK, language bindings, the planned admin UI
//! schema preview) consume these RPCs to discover what they can talk
//! to. A regression here breaks dynamic clients silently — they'd
//! get back empty lists or `Unimplemented` instead of the registry
//! contents. The in-process trait tests cover the data shape; this
//! file pins the wire framing for `repeated CollectionInfo` and the
//! nested `repeated FieldInfo` recursion.

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
    api::content::{
        DescribeCollectionRequest, ListCollectionsRequest, content_api_client::ContentApiClient,
    },
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
        FieldDefinition::builder("body", FieldType::Text).build(),
        FieldDefinition::builder("status", FieldType::Select).build(),
    ];
    def
}

fn make_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("users");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("User".to_string())),
        plural: Some(LocalizedString::Plain("Users".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
    ];
    def.auth = Some(Auth::enabled());
    def
}

fn make_settings_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
    def
}

// ── list_collections_returns_registered_collections_and_globals ──────────

#[tokio::test(flavor = "multi_thread")]
async fn list_collections_returns_registered_collections_and_globals() {
    let ctx = spawn_grpc_server(
        vec![make_posts_def(), make_users_def()],
        vec![make_settings_def()],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let resp = client
        .list_collections(ListCollectionsRequest {})
        .await
        .expect("list_collections")
        .into_inner();

    let coll_slugs: Vec<&str> = resp.collections.iter().map(|c| c.slug.as_str()).collect();
    assert!(
        coll_slugs.contains(&"posts"),
        "should list posts, got: {coll_slugs:?}"
    );
    assert!(
        coll_slugs.contains(&"users"),
        "should list users, got: {coll_slugs:?}"
    );

    let global_slugs: Vec<&str> = resp.globals.iter().map(|g| g.slug.as_str()).collect();
    assert_eq!(
        global_slugs,
        vec!["settings"],
        "should list the settings global"
    );

    // Spot-check the boolean flags survive the wire correctly.
    let users = resp
        .collections
        .iter()
        .find(|c| c.slug == "users")
        .expect("users in list");
    assert!(users.auth, "users collection should be marked auth");
    assert!(users.timestamps, "users collection should have timestamps");

    let posts = resp
        .collections
        .iter()
        .find(|c| c.slug == "posts")
        .expect("posts in list");
    assert!(!posts.auth, "posts collection should NOT be marked auth");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── describe_collection_returns_field_definitions ────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn describe_collection_returns_field_definitions() {
    let ctx = spawn_grpc_server(vec![make_posts_def()], vec![]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let resp = client
        .describe_collection(DescribeCollectionRequest {
            slug: "posts".to_string(),
            is_global: false,
        })
        .await
        .expect("describe_collection")
        .into_inner();

    assert_eq!(resp.slug, "posts");
    assert_eq!(resp.singular_label.as_deref(), Some("Post"));
    assert_eq!(resp.plural_label.as_deref(), Some("Posts"));
    assert!(resp.timestamps);
    assert!(!resp.auth);

    let field_names: Vec<&str> = resp.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        field_names,
        vec!["title", "body", "status"],
        "fields should match definition order"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── describe_global_returns_fields ───────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn describe_global_returns_fields() {
    let ctx = spawn_grpc_server(vec![], vec![make_settings_def()]).await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let resp = client
        .describe_collection(DescribeCollectionRequest {
            slug: "settings".to_string(),
            is_global: true,
        })
        .await
        .expect("describe_collection for global")
        .into_inner();

    assert_eq!(resp.slug, "settings");
    assert!(!resp.timestamps, "globals are always timestamps=false");
    assert!(!resp.auth, "globals are always auth=false");

    let field_names: Vec<&str> = resp.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(field_names, vec!["site_name"]);

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
