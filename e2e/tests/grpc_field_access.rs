//! gRPC e2e: field-level access (read/write denial) over the wire.
//!
//! Field-level access is enforced inside the service layer's write
//! and read paths — fields the caller can't read are stripped from
//! the response before it leaves the server; fields the caller can't
//! write are silently dropped from the incoming `data` before
//! validation. The in-process unit tests assert the helper logic;
//! this file pins that the stripping survives the proto encoder and
//! reaches the wire correctly.

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
use std::collections::HashMap;

use crap_cms::api::content::{DataMap, FieldValue, field_value::Kind};
use tonic::{Request, metadata::MetadataValue};

use crap_cms::{
    api::content::{
        CreateRequest, FindByIdRequest, LoginRequest, UpdateRequest,
        content_api_client::ContentApiClient,
    },
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::spawn_grpc_server_with_lua;

const ACCESS_ADMIN_ONLY: &str = r"
return function(ctx)
    return ctx.user ~= nil and ctx.user.role == 'admin'
end
";

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

/// `posts` with a `secret` field that only admins can read, and a
/// `flag` field that only admins can write. Everyone authenticated
/// can read+write `title`.
fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;

    let title = FieldDefinition::builder("title", FieldType::Text)
        .required(true)
        .build();

    let mut secret = FieldDefinition::builder("secret", FieldType::Text).build();
    secret.access.read = Some(HookRef::new("access.admin_only"));

    let mut flag = FieldDefinition::builder("flag", FieldType::Text).build();
    flag.access.create = Some(HookRef::new("access.admin_only"));
    flag.access.update = Some(HookRef::new("access.admin_only"));

    def.fields = vec![title, secret, flag];
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
) -> String {
    client
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
        .expect("create user");
    client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: email.to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("login")
        .into_inner()
        .token
}

fn get_str(doc: &crap_cms::api::content::Document, field: &str) -> Option<String> {
    doc.fields.as_ref().and_then(|s| {
        s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    })
}

fn has_field(doc: &crap_cms::api::content::Document, field: &str) -> bool {
    doc.fields
        .as_ref()
        .is_some_and(|s| s.fields.contains_key(field))
}

// ── read_denied_field_stripped_from_response_for_non_admin ───────────────

#[tokio::test(flavor = "multi_thread")]
async fn read_denied_field_stripped_from_response_for_non_admin() {
    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), make_posts_def()],
        vec![],
        &[("access/admin_only.lua", ACCESS_ADMIN_ONLY)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let admin_token = create_user_login(&mut client, "a@x.com", "admin").await;
    let viewer_token = create_user_login(&mut client, "v@x.com", "viewer").await;

    // Admin creates a post with title + secret.
    let post_id = client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[("title", "headline"), ("secret", "shhh")])),
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

    // Admin reads — both fields present.
    let admin_view = client
        .find_by_id(with_bearer(
            FindByIdRequest {
                collection: "posts".to_string(),
                id: post_id.clone(),
                ..Default::default()
            },
            &admin_token,
        ))
        .await
        .expect("admin find_by_id")
        .into_inner()
        .document
        .expect("doc");
    assert_eq!(get_str(&admin_view, "title").as_deref(), Some("headline"));
    assert_eq!(
        get_str(&admin_view, "secret").as_deref(),
        Some("shhh"),
        "admin should see the secret field"
    );

    // Viewer reads — `secret` stripped.
    let viewer_view = client
        .find_by_id(with_bearer(
            FindByIdRequest {
                collection: "posts".to_string(),
                id: post_id,
                ..Default::default()
            },
            &viewer_token,
        ))
        .await
        .expect("viewer find_by_id")
        .into_inner()
        .document
        .expect("doc");
    assert_eq!(get_str(&viewer_view, "title").as_deref(), Some("headline"));
    assert!(
        !has_field(&viewer_view, "secret"),
        "viewer must NOT see the secret field; got fields: {:?}",
        viewer_view
            .fields
            .as_ref()
            .map(|s| s.fields.keys().collect::<Vec<_>>())
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── write_denied_field_silently_stripped_on_create ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn write_denied_field_silently_stripped_on_create() {
    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), make_posts_def()],
        vec![],
        &[("access/admin_only.lua", ACCESS_ADMIN_ONLY)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let viewer_token = create_user_login(&mut client, "v2@x.com", "viewer").await;

    // Viewer creates with `flag` set — should succeed but `flag`
    // should be silently stripped (not error, not persisted).
    let post_id = client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[
                    ("title", "viewer-post"),
                    ("flag", "should-be-dropped"),
                ])),
                ..Default::default()
            },
            &viewer_token,
        ))
        .await
        .expect("viewer create should succeed (write-denied fields silently stripped)")
        .into_inner()
        .document
        .expect("doc")
        .id;

    // Read back as viewer — flag should be empty/absent.
    let view = client
        .find_by_id(with_bearer(
            FindByIdRequest {
                collection: "posts".to_string(),
                id: post_id,
                ..Default::default()
            },
            &viewer_token,
        ))
        .await
        .expect("find_by_id")
        .into_inner()
        .document
        .expect("doc");
    assert_eq!(get_str(&view, "title").as_deref(), Some("viewer-post"));
    let flag = get_str(&view, "flag").unwrap_or_default();
    assert!(
        flag.is_empty(),
        "write-denied 'flag' should not have been persisted, got: {flag:?}"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── write_denied_field_silently_stripped_on_update ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn write_denied_field_silently_stripped_on_update() {
    let ctx = spawn_grpc_server_with_lua(
        vec![make_users_def(), make_posts_def()],
        vec![],
        &[("access/admin_only.lua", ACCESS_ADMIN_ONLY)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let admin_token = create_user_login(&mut client, "a3@x.com", "admin").await;
    let viewer_token = create_user_login(&mut client, "v3@x.com", "viewer").await;

    // Admin creates with flag=admin-set.
    let post_id = client
        .create(with_bearer(
            CreateRequest {
                events: None,
                collection: "posts".to_string(),
                data: Some(proto_struct(&[
                    ("title", "Original"),
                    ("flag", "admin-set"),
                ])),
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

    // Viewer attempts to overwrite flag.
    client
        .update(with_bearer(
            UpdateRequest {
                events: None,
                collection: "posts".to_string(),
                id: post_id.clone(),
                data: Some(proto_struct(&[("flag", "viewer-tampered")])),
                ..Default::default()
            },
            &viewer_token,
        ))
        .await
        .expect("viewer update should succeed (denied fields silently stripped)");

    // Admin reads back — flag should still be admin-set.
    let after = client
        .find_by_id(with_bearer(
            FindByIdRequest {
                collection: "posts".to_string(),
                id: post_id,
                ..Default::default()
            },
            &admin_token,
        ))
        .await
        .expect("admin find_by_id")
        .into_inner()
        .document
        .expect("doc");
    assert_eq!(
        get_str(&after, "flag").as_deref(),
        Some("admin-set"),
        "flag must NOT have been overwritten by the viewer"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
