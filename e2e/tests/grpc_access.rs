//! gRPC e2e: collection-level access functions over the wire.
//!
//! All previous gRPC e2e tests ran against a registry with no
//! access fns — every collection was effectively wide-open. This
//! file plants real Lua access functions via
//! `spawn_grpc_server_with_lua` and verifies their decisions surface
//! over the wire as the right gRPC status codes:
//!   - access denied → `PERMISSION_DENIED`
//!   - constrained-access where-filter → reduced `Find` result set
//!
//! Existing in-process trait tests (`tests/grpc_default_deny.rs`)
//! cover the same surface but never go through a real `tonic`
//! channel.

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

use crap_cms::core::HookRef;
use std::collections::BTreeMap;

use prost_types::{Struct, Value, value::Kind};
use tonic::{Code, Request, metadata::MetadataValue};

use crap_cms::{
    api::content::{
        CreateRequest, DeleteRequest, FindRequest, LoginRequest, UpdateRequest,
        content_api_client::ContentApiClient,
    },
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::spawn_grpc_server_with_lua;

// ── Lua fixtures ─────────────────────────────────────────────────────────

const ACCESS_AUTHENTICATED: &str = r"
return function(ctx)
    return ctx.user ~= nil
end
";

const ACCESS_NEVER: &str = r"
return function(_ctx)
    return false
end
";

const ACCESS_ADMIN_ONLY: &str = r"
return function(ctx)
    return ctx.user ~= nil and ctx.user.role == 'admin'
end
";

/// Constrained-access fn: returns `true` for admins, otherwise a
/// where-filter restricting the user to rows they authored
/// (`author_id == ctx.user.id`).
const ACCESS_OWN_ROWS: &str = r"
return function(ctx)
    if ctx.user == nil then return false end
    if ctx.user.role == 'admin' then return true end
    return { author_id = { equals = ctx.user.id } }
end
";

// ── Fixtures ─────────────────────────────────────────────────────────────

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
        FieldDefinition::builder("name", FieldType::Text).build(),
        FieldDefinition::builder("role", FieldType::Text).build(),
    ];
    def.auth = Some(Auth::enabled());
    def
}

fn make_restricted_posts_def() -> CollectionDefinition {
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
    def.access = Access {
        read: Some(HookRef::new("access.authenticated")),
        create: Some(HookRef::new("access.admin_only")),
        update: Some(HookRef::new("access.admin_only")),
        delete: Some(HookRef::new("access.admin_only")),
        ..Default::default()
    };
    def
}

fn make_owned_posts_def() -> CollectionDefinition {
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
        FieldDefinition::builder("author_id", FieldType::Text).build(),
    ];
    def.access = Access {
        read: Some(HookRef::new("access.own_rows")),
        ..Default::default()
    };
    def
}

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

fn with_bearer<T>(req: T, token: &str) -> Request<T> {
    let mut r = Request::new(req);
    let bearer: MetadataValue<_> = format!("Bearer {token}").parse().expect("valid metadata");
    r.metadata_mut().insert("authorization", bearer);
    r
}

async fn create_user_login(
    client: &mut ContentApiClient<tonic::transport::Channel>,
    email: &str,
    role: &str,
) -> (String, String) {
    let id = client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", email),
                ("name", email),
                ("role", role),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user")
        .into_inner()
        .document
        .expect("doc")
        .id;
    let token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: email.to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("login")
        .into_inner()
        .token;
    (id, token)
}

// ── read_access_denied_anonymous_allowed_authenticated ───────────────────

#[tokio::test(flavor = "multi_thread")]
async fn read_access_denied_anonymous_allowed_authenticated() {
    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), make_restricted_posts_def()],
        vec![],
        &[
            ("access/authenticated.lua", ACCESS_AUTHENTICATED),
            ("access/admin_only.lua", ACCESS_ADMIN_ONLY),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (_admin_id, admin_token) = create_user_login(&mut client, "a@x.com", "admin").await;

    // Admin creates a post so there's something to read.
    client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[("title", "secret")])),
                ..Default::default()
            },
            &admin_token,
        ))
        .await
        .expect("admin create");

    // Anonymous Find → PERMISSION_DENIED (access.authenticated returns false for ctx.user == nil).
    let denied = client
        .find(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect_err("anonymous find should be denied");
    assert_eq!(
        denied.code(),
        Code::PermissionDenied,
        "anonymous find → PERMISSION_DENIED, got {:?}: {}",
        denied.code(),
        denied.message()
    );

    // Authenticated Find → succeeds, returns the doc.
    let resp = client
        .find(with_bearer(
            FindRequest {
                collection: "posts".to_string(),
                ..Default::default()
            },
            &admin_token,
        ))
        .await
        .expect("authenticated find")
        .into_inner();
    assert_eq!(resp.documents.len(), 1);

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── create_access_denies_non_admin_role ──────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn create_access_denies_non_admin_role() {
    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), make_restricted_posts_def()],
        vec![],
        &[
            ("access/authenticated.lua", ACCESS_AUTHENTICATED),
            ("access/admin_only.lua", ACCESS_ADMIN_ONLY),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (_viewer_id, viewer_token) = create_user_login(&mut client, "v@x.com", "viewer").await;

    let denied = client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[("title", "should fail")])),
                ..Default::default()
            },
            &viewer_token,
        ))
        .await
        .expect_err("viewer create should be denied");
    assert_eq!(
        denied.code(),
        Code::PermissionDenied,
        "viewer create → PERMISSION_DENIED, got {:?}",
        denied.code()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── update_and_delete_access_deny_non_admin ──────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn update_and_delete_access_deny_non_admin() {
    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), make_restricted_posts_def()],
        vec![],
        &[
            ("access/authenticated.lua", ACCESS_AUTHENTICATED),
            ("access/admin_only.lua", ACCESS_ADMIN_ONLY),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (_admin_id, admin_token) = create_user_login(&mut client, "a2@x.com", "admin").await;
    let (_viewer_id, viewer_token) = create_user_login(&mut client, "v2@x.com", "viewer").await;

    let post_id = client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[("title", "Original")])),
                ..Default::default()
            },
            &admin_token,
        ))
        .await
        .expect("admin create")
        .into_inner()
        .document
        .expect("doc")
        .id;

    let update_denied = client
        .update(with_bearer(
            UpdateRequest {
                events: None,
                collection: "posts".to_string(),
                id: post_id.clone(),
                data: Some(proto_struct(&[("title", "Hacked")])),
                ..Default::default()
            },
            &viewer_token,
        ))
        .await
        .expect_err("viewer update should be denied");
    assert_eq!(update_denied.code(), Code::PermissionDenied);

    let delete_denied = client
        .delete(with_bearer(
            DeleteRequest {
                events: None,
                collection: "posts".to_string(),
                id: post_id,
                force_hard_delete: false,
            },
            &viewer_token,
        ))
        .await
        .expect_err("viewer delete should be denied");
    assert_eq!(delete_denied.code(), Code::PermissionDenied);

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── never_access_blocks_all_reads ────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn never_access_blocks_all_reads() {
    let mut posts_def = make_restricted_posts_def();
    posts_def.access.read = Some(HookRef::new("access.never"));

    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), posts_def],
        vec![],
        &[
            ("access/never.lua", ACCESS_NEVER),
            ("access/admin_only.lua", ACCESS_ADMIN_ONLY),
        ],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (_admin_id, admin_token) = create_user_login(&mut client, "a3@x.com", "admin").await;

    let denied = client
        .find(with_bearer(
            FindRequest {
                collection: "posts".to_string(),
                ..Default::default()
            },
            &admin_token,
        ))
        .await
        .expect_err("never-access blocks even admin");
    assert_eq!(denied.code(), Code::PermissionDenied);

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── constrained_access_filters_rows_to_owner ─────────────────────────────
//
// `access.own_rows` returns a where-filter `{author_id = ctx.user.id}`
// for non-admins. Two non-admin users each seed a row; each Find
// returns only their own.

#[tokio::test(flavor = "multi_thread")]
async fn constrained_access_filters_rows_to_owner() {
    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), make_owned_posts_def()],
        vec![],
        &[("access/own_rows.lua", ACCESS_OWN_ROWS)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let (alice_id, alice_token) = create_user_login(&mut client, "alice@x.com", "editor").await;
    let (bob_id, bob_token) = create_user_login(&mut client, "bob@x.com", "editor").await;

    // Each user creates their own post. Access fn is also gating
    // *write*, but the access fn returns a filter (truthy) → write
    // is allowed for both users.
    client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[
                    ("title", "alice doc"),
                    ("author_id", &alice_id),
                ])),
                ..Default::default()
            },
            &alice_token,
        ))
        .await
        .expect("alice create");
    client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[
                    ("title", "bob doc"),
                    ("author_id", &bob_id),
                ])),
                ..Default::default()
            },
            &bob_token,
        ))
        .await
        .expect("bob create");

    let alice_view = client
        .find(with_bearer(
            FindRequest {
                collection: "posts".to_string(),
                ..Default::default()
            },
            &alice_token,
        ))
        .await
        .expect("alice find")
        .into_inner();
    assert_eq!(
        alice_view.documents.len(),
        1,
        "alice should only see her own row, got: {:?}",
        alice_view
            .documents
            .iter()
            .filter_map(|d| d.fields.as_ref())
            .filter_map(|s| s.fields.get("title"))
            .filter_map(|v| match &v.kind {
                Some(Kind::StringValue(s)) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
    );

    let bob_view = client
        .find(with_bearer(
            FindRequest {
                collection: "posts".to_string(),
                ..Default::default()
            },
            &bob_token,
        ))
        .await
        .expect("bob find")
        .into_inner();
    assert_eq!(bob_view.documents.len(), 1, "bob sees one row (his own)");

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
