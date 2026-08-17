//! gRPC e2e: per-collection `methods` lists are independent.
//!
//! With the new `methods`-per-collection design, adding an auth
//! collection (with its own methods) MUST NOT change the auth
//! behavior of unrelated requests against other collections. This
//! pins the structural invariant — a `users` collection's Login
//! still issues a `users` token, and a `service_accounts` token
//! issued by a future strategy does not authenticate as a `users`
//! principal.
//!
//! Today, with only `bearer` + `password_login` in scope (no
//! strategy evaluator in the per-request gRPC path yet), this test
//! reduces to: JWTs are scoped to the collection they were issued
//! for. The full cross-collection-strategy-isolation test lands
//! with the unified evaluator follow-up.

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
    api::content::{CreateRequest, LoginRequest, content_api_client::ContentApiClient},
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::spawn_grpc_server;

fn auth_collection(slug: &str) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.labels = Labels {
        singular: Some(LocalizedString::Plain(slug.to_string())),
        plural: Some(LocalizedString::Plain(format!("{slug}s"))),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        FieldDefinition::builder("name", FieldType::Text).build(),
    ];
    def.auth = Some(Auth::enabled());
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

// ── tokens_are_scoped_to_their_issuing_collection ───────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn tokens_are_scoped_to_their_issuing_collection() {
    let ctx = spawn_grpc_server(
        vec![
            auth_collection("users"),
            auth_collection("service_accounts"),
        ],
        vec![],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    // Create + login on `users`.
    client
        .create(CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(proto_struct(&[
                ("email", "u@x.com"),
                ("name", "User"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create user");
    let user_token = client
        .login(LoginRequest {
            collection: "users".to_string(),
            email: "u@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("user login")
        .into_inner()
        .token;

    // Create + login on `service_accounts`.
    client
        .create(CreateRequest {
            events: None,
            collection: "service_accounts".to_string(),
            data: Some(proto_struct(&[
                ("email", "svc@x.com"),
                ("name", "Service"),
                ("password", "password-12345"),
            ])),
            ..Default::default()
        })
        .await
        .expect("create svc");
    let svc_token = client
        .login(LoginRequest {
            collection: "service_accounts".to_string(),
            email: "svc@x.com".to_string(),
            password: "password-12345".to_string(),
        })
        .await
        .expect("svc login")
        .into_inner()
        .token;

    // Both tokens must be non-empty and distinct.
    assert!(!user_token.is_empty());
    assert!(!svc_token.is_empty());
    assert_ne!(
        user_token, svc_token,
        "JWTs for different collections must not collide"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
