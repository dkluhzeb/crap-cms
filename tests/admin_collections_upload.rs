//! Upload-collection integration tests for the admin HTTP handlers.
//!
//! Covers: the upload API (create / update / delete, bearer and cookie auth,
//! MIME rejection, access filters), the upload create/edit forms, and the
//! failure and forgery guards of an upload update.

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

mod admin_collections_support;

use std::fs;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

use crap_cms::{
    core::{
        auth,
        collection::{CollectionDefinition, Labels},
        field::{FieldAdmin, FieldDefinition, FieldType, LocalizedString},
    },
    db::DbConnection,
    service::{ServiceContext, auth::lock_user},
};

use admin_collections_support::{
    TEST_CSRF, TestApp, auth_and_csrf, body_string, create_test_user, make_auth_cookie,
    make_posts_def, make_users_def, setup_app, write_access_hooks,
};

fn csrf_cookie() -> String {
    format!("crap_csrf={TEST_CSRF}")
}

fn make_bearer_token(app: &TestApp, user_id: &str, email: &str) -> String {
    // Match the user's current session version, as `make_auth_cookie` does:
    // `update_password` bumps it, and a stale token is rejected.
    let conn = app.pool.get().unwrap();
    let session_version =
        crap_cms::db::query::auth::get_session_version(&conn, "users", user_id).unwrap_or(0);
    drop(conn);
    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .session_version(session_version)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("Bearer {token}")
}

fn make_media_def() -> CollectionDefinition {
    use crap_cms::core::upload::CollectionUpload;

    fn hidden_text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build()
    }
    fn hidden_number(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Number)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build()
    }

    let mut def = CollectionDefinition::new("media");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Media".to_string())),
        plural: Some(LocalizedString::Plain("Media".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .admin(FieldAdmin::builder().readonly(true).build())
            .build(),
        hidden_text("mime_type"),
        hidden_number("filesize"),
        hidden_number("width"),
        hidden_number("height"),
        hidden_text("url"),
        FieldDefinition::builder("alt", FieldType::Text).build(),
    ];
    def.upload = Some(CollectionUpload {
        enabled: true,
        mime_types: vec!["image/*".to_string(), "application/pdf".to_string()],
        ..Default::default()
    });
    def
}

fn build_multipart_body(
    filename: &str,
    content_type: &str,
    file_data: &[u8],
    fields: &[(&str, &str)],
) -> (String, Vec<u8>) {
    let boundary = "----CrapTestBoundary";
    let mut body = Vec::new();

    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"_file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(file_data);
    body.extend_from_slice(b"\r\n");

    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let content_type = format!("multipart/form-data; boundary={boundary}");
    (content_type, body)
}

fn tiny_png() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::png::PngEncoder::new(&mut buf);
    use image::ImageEncoder;
    encoder
        .write_image(&[0u8, 0, 0, 0], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    buf.into_inner()
}

/// Multipart body with only text fields (no `_file` part) — an upload update
/// that changes metadata without replacing the file.
fn build_fields_only_multipart(fields: &[(&str, &str)]) -> (String, Vec<u8>) {
    let boundary = "----CrapTestBoundary";
    let mut body = Vec::new();

    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[tokio::test]
async fn upload_api_create_returns_201_with_document() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("photo.png", "image/png", &png, &[("alt", "Test alt")]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["document"]["id"].is_string());
    assert_eq!(json["document"]["alt"], "Test alt");
    assert!(
        json["document"]["filename"]
            .as_str()
            .unwrap()
            .ends_with("photo.png")
    );

    // Upload auto-injected fields use `admin.hidden = true` (admin-form-only) —
    // they remain in API responses so consumers (gRPC, Lua, MCP, admin upload
    // preview widget, focal-point selector) can render previews and crops.
    // The strict "strip from API" semantic lives on top-level `hidden = true`,
    // which upload meta fields do NOT set.
    assert!(
        json["document"]["url"].is_string(),
        "url must be in API response (admin.hidden does not strip from API)"
    );
    assert_eq!(json["document"]["mime_type"], "image/png");
    assert!(json["document"]["filesize"].is_number());
    assert!(json["document"]["width"].is_number());
    assert!(json["document"]["height"].is_number());
}

/// Regression for the upload-edit form bug: after uploading an image to a
/// media collection, the admin edit page must render the `<crap-focal-point>`
/// preview block. The block is gated on `upload.preview` being set, which is
/// derived from the `url` + `mime_type` fields the document carries — so this
/// test fails the moment those fields get stripped from the service-layer
/// response again (e.g. by re-introducing `admin.hidden` → API stripping).
#[tokio::test]
async fn admin_upload_edit_form_renders_focal_point_preview() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploaduiedit@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploaduiedit@test.com");
    let cookie = make_auth_cookie(&app, &user_id, "uploaduiedit@test.com");

    // Upload an image via the upload API.
    let png = tiny_png();
    let (ct, body) = build_multipart_body("photo.png", "image/png", &png, &[("alt", "preview")]);
    let upload_resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload_resp.status(), StatusCode::CREATED);
    let upload_json: serde_json::Value =
        serde_json::from_str(&body_string(upload_resp.into_body()).await).unwrap();
    let doc_id = upload_json["document"]["id"].as_str().unwrap().to_string();

    // GET the admin edit form for the uploaded doc.
    let edit_resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/media/{doc_id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(edit_resp.status(), StatusCode::OK);

    let html = body_string(edit_resp.into_body()).await;
    assert!(
        html.contains("<crap-focal-point"),
        "edit page must render the focal-point preview widget; if this fails, \
         upload meta fields are being stripped from the service response again"
    );
    assert!(
        html.contains("src=\"/uploads/"),
        "preview widget must point at the uploaded image"
    );
}

#[tokio::test]
async fn upload_api_create_no_file_returns_400() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let boundary = "----CrapTestBoundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"alt\"\r\n\r\nsome text\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/media")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("No file"));
}

#[tokio::test]
async fn upload_api_create_non_upload_collection_returns_400() {
    let app = setup_app(vec![make_users_def(), make_posts_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("photo.png", "image/png", &png, &[]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/posts")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .contains("not an upload collection")
    );
}

#[tokio::test]
async fn upload_api_create_unknown_collection_returns_404() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("photo.png", "image/png", &png, &[]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/nonexistent")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_api_create_rejected_mime_returns_400() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let (ct, body) = build_multipart_body("notes.txt", "text/plain", b"hello world", &[]);

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("not allowed"));
}

#[tokio::test]
async fn upload_api_update_replaces_file() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("first.png", "image/png", &png, &[("alt", "First")]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", &ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let create_body = body_string(resp.into_body()).await;
    let create_json: serde_json::Value = serde_json::from_str(&create_body).unwrap();
    let doc_id = create_json["document"]["id"].as_str().unwrap();
    let old_filename = create_json["document"]["filename"]
        .as_str()
        .unwrap()
        .to_string();

    let png2 = tiny_png();
    let (ct2, body2) = build_multipart_body("second.png", "image/png", &png2, &[("alt", "Second")]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::patch(format!("/api/upload/media/{doc_id}"))
                .header("content-type", ct2)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body2))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let update_body = body_string(resp.into_body()).await;
    let update_json: serde_json::Value = serde_json::from_str(&update_body).unwrap();
    let new_filename = update_json["document"]["filename"].as_str().unwrap();
    assert_ne!(
        new_filename, old_filename,
        "Filename should change on file replacement"
    );
    assert_eq!(update_json["document"]["alt"], "Second");
}

#[tokio::test]
async fn upload_api_delete_returns_success() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("todelete.png", "image/png", &png, &[]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let create_body = body_string(resp.into_body()).await;
    let create_json: serde_json::Value = serde_json::from_str(&create_body).unwrap();
    let doc_id = create_json["document"]["id"].as_str().unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::delete(format!("/api/upload/media/{doc_id}"))
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let del_body = body_string(resp.into_body()).await;
    let del_json: serde_json::Value = serde_json::from_str(&del_body).unwrap();
    assert_eq!(del_json["success"], true);
}

/// A token whose account is locked is refused, not treated as an anonymous
/// request the collection's access rules may still allow.
#[tokio::test]
async fn upload_api_rejects_a_locked_accounts_token() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "locked@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "locked@test.com");

    // Lock the account the way every surface does: the flag plus a session
    // version bump that retires the tokens it already holds.
    {
        let conn = app.pool.get().unwrap();
        let ctx = ServiceContext::slug_only("users").conn(&conn).build();
        lock_user(&ctx, &user_id).unwrap();
    }

    let png = tiny_png();
    let (ct, body) = build_multipart_body("locked.png", "image/png", &png, &[]);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Under a delete rule that returns a filter, deleting an upload outside the
/// filter and deleting one that doesn't exist answer alike — otherwise the
/// response reveals which ids exist.
#[tokio::test]
async fn upload_api_delete_does_not_reveal_existence_under_a_filter_rule() {
    let mut media = make_media_def();
    media.access.delete = Some("hooks.access.no_match".into());
    let app = setup_app(vec![make_users_def(), media], vec![]);
    write_access_hooks(
        app.tmp.path(),
        "function M.no_match(ctx)\n    return { filename = \"never-matches\" }\nend",
    );
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("kept.png", "image/png", &png, &[]);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created: serde_json::Value =
        serde_json::from_str(&body_string(resp.into_body()).await).unwrap();
    let existing = created["document"]["id"].as_str().unwrap().to_string();

    let mut statuses = Vec::new();
    for id in [existing.as_str(), "nonexistent-id"] {
        let resp = app
            .router
            .clone()
            .oneshot(
                Request::delete(format!("/api/upload/media/{id}"))
                    .header("authorization", &bearer)
                    .header("Cookie", csrf_cookie())
                    .header("X-CSRF-Token", TEST_CSRF)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        statuses.push(resp.status());
    }

    assert_ne!(
        statuses[0],
        StatusCode::OK,
        "the filter excludes the upload"
    );
    assert_eq!(
        statuses[0], statuses[1],
        "an excluded upload and a missing one must answer alike"
    );
}

#[tokio::test]
async fn upload_api_delete_nonexistent_returns_404() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/api/upload/media/nonexistent-id")
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_collection_create_form_shows_file_field() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "upform@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "upform@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/media/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("file") || body.contains("upload"),
        "Upload collection create form should contain file upload controls"
    );
}

#[tokio::test]
async fn upload_collection_create_form_has_upload_context() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploadadm@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "uploadadm@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/media/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Regression: a DB error while loading the OLD document during an upload
/// update was silently swallowed (`pool.get().ok()… find_by_id(…).ok()`), so
/// the new file was stored, the write went through with no old-file cleanup,
/// and the OLD file was orphaned. The upload must now fail BEFORE storing.
///
/// The read is broken via a dropped JOIN TABLE (`media_gallery`) — dropping
/// a column wouldn't error: `SQLite`'s double-quoted-string fallback turns
/// `SELECT "gone_col"` into a string literal instead of a failure.
#[tokio::test]
async fn upload_update_fails_before_storing_when_old_doc_read_errors() {
    fn count_files(dir: &std::path::Path) -> usize {
        let Ok(entries) = fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|e| {
                let path = e.path();
                if path.is_dir() { count_files(&path) } else { 1 }
            })
            .sum()
    }

    let mut media = make_media_def();
    media.fields.push(
        FieldDefinition::builder("gallery", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("caption", FieldType::Text).build(),
            ])
            .build(),
    );
    let app = setup_app(vec![media, make_users_def()], vec![]);
    let user_id = create_test_user(&app, "orphan@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "orphan@test.com");

    // Seed a media document directly.
    let conn = app.pool.get().unwrap();
    conn.execute(
        "INSERT INTO media (id, filename, url) VALUES ('m1', 'old.png', '/uploads/media/old.png')",
        &[],
    )
    .unwrap();

    // Break the old-document read: drop the array join table the definition
    // still expects, so `find_by_id`'s hydration errors while every other
    // query in the flow keeps working.
    conn.execute("DROP TABLE media_gallery", &[]).unwrap();
    drop(conn);

    let uploads_dir = app.tmp.path().join("uploads");
    let before = count_files(&uploads_dir);

    let (content_type, body) =
        build_multipart_body("new.png", "image/png", &tiny_png(), &[("alt", "new")]);
    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/media/m1")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        !resp.status().is_redirection(),
        "upload update must not report success when the old-doc read fails, got {}",
        resp.status()
    );
    assert_eq!(
        count_files(&uploads_dir),
        before,
        "upload must not store a file when the old-document read fails"
    );
}

/// Security regression: the server-derived `url` column must not be settable
/// from user input. An upload document's `url` is what the serve access gate
/// matches to authorize the file bytes; if a caller could forge it, they could
/// point their own (readable) document at another document's file path and read
/// it through the gate. A no-file update carrying a forged `url` must leave the
/// stored value unchanged while still applying legitimate field edits.
#[tokio::test]
async fn upload_update_cannot_forge_the_url_column() {
    let app = setup_app(vec![make_users_def(), make_media_def()], vec![]);
    let user_id = create_test_user(&app, "uploader@test.com", "secret123");
    let bearer = make_bearer_token(&app, &user_id, "uploader@test.com");

    let png = tiny_png();
    let (ct, body) = build_multipart_body("real.png", "image/png", &png, &[("alt", "Real")]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header("content-type", &ct)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let create_json: serde_json::Value =
        serde_json::from_str(&body_string(resp.into_body()).await).unwrap();
    let doc_id = create_json["document"]["id"].as_str().unwrap().to_string();
    let real_url = create_json["document"]["url"]
        .as_str()
        .expect("created upload has a server-derived url")
        .to_string();

    // A no-file update that tries to forge `url` at a victim's file path while
    // legitimately editing `alt`.
    let (ct2, body2) = build_fields_only_multipart(&[
        ("url", "/uploads/media/victim-file.png"),
        ("alt", "Updated"),
    ]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::patch(format!("/api/upload/media/{doc_id}"))
                .header("content-type", ct2)
                .header("authorization", &bearer)
                .header("Cookie", csrf_cookie())
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(body2))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let update_json: serde_json::Value =
        serde_json::from_str(&body_string(resp.into_body()).await).unwrap();

    assert_eq!(
        update_json["document"]["url"].as_str(),
        Some(real_url.as_str()),
        "the forged url must be ignored — the stored server-derived url stays put"
    );
    assert_eq!(
        update_json["document"]["alt"], "Updated",
        "a legitimate field edit still applies"
    );
}
