//! gRPC e2e: Lua hook error → gRPC `Status` mapping.
//!
//! Hooks can fail three ways, and each maps to a distinct gRPC
//! status code:
//!   - Lua `error("…")` from a lifecycle hook → `INVALID_ARGUMENT`
//!     (hook errors are user-recoverable per the
//!     `ServiceError::classify` "hook error:" pattern).
//!   - Access fn raising an error → `PERMISSION_DENIED` (treated
//!     as denied; the `?` propagates `ServiceError::AccessDenied`).
//!   - Structured per-field `ValidationError` → `INVALID_ARGUMENT`
//!     with field name in the response.
//!
//! `Internal` would mean a server bug; the assertions here pin
//! the user-recoverable mappings so a future refactor of the
//! error pipeline can't silently demote them.

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
use tonic::Code;

use crap_cms::{
    api::content::{CreateRequest, FindRequest, content_api_client::ContentApiClient},
    core::{
        collection::*,
        field::{FieldDefinition, FieldType, LocalizedString},
    },
};
use crap_cms_e2e::spawn_grpc_server_with_lua;

// ── Lua fixtures ─────────────────────────────────────────────────────────

/// `before_change` hook that always errors. Should surface as
/// `INVALID_ARGUMENT` because `ServiceError::classify` catches the
/// "hook error:" / "runtime error:" prefix and routes it to
/// `ServiceError::HookError`, which maps to `Status::invalid_argument`.
const ALWAYS_ERROR_HOOK: &str = r#"
local M = {}
function M.boom(_ctx)
    error("hook error: boom")
end
return M
"#;

/// Access fn that raises a Lua error. The `HookRunner`'s
/// `check_access` treats errors as "denied" — wraps them into
/// `ServiceError::AccessDenied` → `Status::permission_denied`.
const EXPLODING_ACCESS: &str = r#"
return function(_ctx)
    error("access fn explosion")
end
"#;

/// `before_validate` hook that raises a structured `FieldError` via
/// `crap.validation_error`. Maps to `ServiceError::Validation` →
/// `Status::invalid_argument` with the field name in the message.
const FIELD_ERROR_HOOK: &str = r#"
local M = {}
function M.required_title(ctx)
    if ctx.data and (ctx.data.title == nil or ctx.data.title == "") then
        crap.validation_error({ title = "title is required" })
    end
    return ctx
end
return M
"#;

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

fn posts_def_with_hooks(hooks: Hooks) -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    def.timestamps = true;
    def.hooks = hooks;
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    def
}

// ── lifecycle_hook_error_maps_to_invalid_argument ────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_hook_error_maps_to_invalid_argument() {
    let def = posts_def_with_hooks(Hooks {
        before_change: vec!["hooks.boom.boom".to_string()],
        ..Default::default()
    });

    let ctx =
        spawn_grpc_server_with_lua(vec![def], vec![], &[("hooks/boom.lua", ALWAYS_ERROR_HOOK)])
            .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(proto_struct(&[("title", "anything")])),
            ..Default::default()
        })
        .await
        .expect_err("hook that errors must fail the request");

    assert_eq!(
        status.code(),
        Code::InvalidArgument,
        "Lua hook error → INVALID_ARGUMENT, got {:?}: {}",
        status.code(),
        status.message()
    );
    assert_ne!(
        status.code(),
        Code::Internal,
        "user-recoverable hook errors must NOT surface as Internal"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── access_fn_error_maps_to_permission_denied ────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn access_fn_error_maps_to_permission_denied() {
    let mut def = posts_def_with_hooks(Hooks::default());
    def.access = Access {
        read: Some("access.exploding".to_string()),
        ..Default::default()
    };

    let ctx = spawn_grpc_server_with_lua(
        vec![def],
        vec![],
        &[("access/exploding.lua", EXPLODING_ACCESS)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .find(FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        })
        .await
        .expect_err("Find with exploding access fn must fail");

    assert_eq!(
        status.code(),
        Code::PermissionDenied,
        "errored access fn → PERMISSION_DENIED (treated as denied), got {:?}: {}",
        status.code(),
        status.message()
    );
    assert_ne!(
        status.code(),
        Code::Internal,
        "access fn errors must NOT leak as server errors"
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}

// ── structured_validation_error_includes_field_name ──────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn structured_validation_error_includes_field_name() {
    let def = posts_def_with_hooks(Hooks {
        before_validate: vec!["hooks.field_validator.required_title".to_string()],
        ..Default::default()
    });

    let ctx = spawn_grpc_server_with_lua(
        vec![def],
        vec![],
        &[("hooks/field_validator.lua", FIELD_ERROR_HOOK)],
    )
    .await;
    let mut client = ContentApiClient::new(ctx.channel.clone());

    let status = client
        .create(CreateRequest {
            collection: "posts".to_string(),
            data: Some(Struct {
                fields: BTreeMap::new(),
            }),
            ..Default::default()
        })
        .await
        .expect_err("structured validation error must fail the request");

    assert_eq!(
        status.code(),
        Code::InvalidArgument,
        "structured ValidationError → INVALID_ARGUMENT, got {:?}: {}",
        status.code(),
        status.message()
    );
    assert!(
        status.message().to_lowercase().contains("title"),
        "error message should mention the offending field, got: {}",
        status.message()
    );

    ctx.shutdown.cancel();
    let _ = ctx.server_handle.await;
}
