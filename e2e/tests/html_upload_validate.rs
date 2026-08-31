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

//! Validate dry-run on an UPLOAD collection — the pre-upload form validate
//! must not fail on the server-managed system fields.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::upload::CollectionUpload;
use serde_json::{Value, json};
use tower::ServiceExt;

use crap_cms_e2e::helpers::*;

fn make_media_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
        FieldDefinition::builder("width", FieldType::Number).build(),
        FieldDefinition::builder("height", FieldType::Number).build(),
        FieldDefinition::builder("filesize", FieldType::Number).build(),
        FieldDefinition::builder("caption", FieldType::Text).build(),
    ];
    def.upload = Some(CollectionUpload::new());
    def
}

/// Regression: the validate dry-run injected the string placeholder
/// `_pending_upload` into NUMBER-typed upload system fields
/// (width/height/filesize/focal/size variants); strict number validation
/// then failed every media validate with `validation.invalid_number`,
/// making the admin upload form unsubmittable. Number system fields are
/// now omitted from the dry-run payload.
#[tokio::test]
async fn upload_collection_validate_passes_without_file_metadata() {
    let ctx = setup_html_test(
        vec![make_users_def(), make_media_def()],
        vec![],
        "uploadval@test.com",
        "pass123",
    );

    let resp = ctx
        .app
        .router
        .clone()
        .oneshot(
            Request::post("/admin/collections/media/validate")
                .header("Cookie", auth_and_csrf(&ctx.cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("Content-Type", "application/json")
                .body(Body::from(
                    json!({ "data": { "caption": "hello" } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = body_string(resp.into_body()).await;
    let parsed: Value = serde_json::from_str(&body).expect("json response");

    assert_eq!(
        parsed.get("valid").and_then(Value::as_bool),
        Some(true),
        "pre-upload validate must pass; got: {body}"
    );
}
