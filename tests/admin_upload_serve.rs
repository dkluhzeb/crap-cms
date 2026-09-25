//! Upload-serving integration tests for the admin HTTP router.
//!
//! Covers: serving stored files (MIME, cache control), path traversal, and the
//! per-document serve gate (default deny, trash, row constraints, drafts,
//! localized collections).

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

mod admin_globals_support;

use std::fs;

use axum::{
    body::Body,
    http::{Request, StatusCode, header::COOKIE},
};
use tower::ServiceExt;

use admin_globals_support::{
    body_string, create_test_user, make_auth_cookie, make_locale_config, make_posts_def,
    make_users_def, setup_app, setup_app_in_dir, setup_app_with_config, tiny_png,
};
use crap_cms::{
    config::CrapConfig,
    core::{
        HookRef,
        collection::{CollectionDefinition, VersionsConfig},
        field::{FieldDefinition, FieldType},
        upload::CollectionUpload,
    },
    db::{DbConnection, DbValue, query},
};

#[tokio::test]
async fn serve_upload_nonexistent_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/nofile.jpg")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serve_upload_path_traversal_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/uploads/posts/../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn serve_upload_existing_file() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let upload_dir = app._tmp.path().join("uploads").join("posts");
    fs::create_dir_all(&upload_dir).unwrap();
    fs::write(upload_dir.join("test.txt"), b"hello world").unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/test.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .map_or("", |v| v.to_str().unwrap_or(""));
    assert!(
        ct.contains("text/plain"),
        "Should detect text/plain MIME, got {ct}"
    );
    let cache = resp
        .headers()
        .get("cache-control")
        .map_or("", |v| v.to_str().unwrap_or(""));
    assert!(
        cache.contains("public"),
        "Public file should have public cache control, got {cache}"
    );
    let body = body_string(resp.into_body()).await;
    assert_eq!(body, "hello world");
}

#[tokio::test]
async fn serve_upload_image_file() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let upload_dir = app._tmp.path().join("uploads").join("posts");
    fs::create_dir_all(&upload_dir).unwrap();
    let png_data = tiny_png();
    fs::write(upload_dir.join("image.png"), &png_data).unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/image.png")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .map_or("", |v| v.to_str().unwrap_or(""));
    assert!(
        ct.contains("image/png"),
        "Should detect image/png MIME, got {ct}"
    );
}

#[tokio::test]
async fn serve_upload_path_traversal_blocked() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/uploads/posts/../../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// An upload collection with a *localized* field (stored per-locale as
/// `caption__en`/`caption__de`) and no fast public path (`soft_delete` forces
/// the per-document visibility find).
fn make_localized_upload_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("media");
    def.timestamps = true;
    def.soft_delete = true;
    def.upload = Some(CollectionUpload::new());

    let mut caption = FieldDefinition::builder("caption", FieldType::Text).build();
    caption.localized = true;
    def.fields = vec![
        // Upload system columns are normally injected by the Lua parser
        // (`inject_upload_fields`); add the ones this test touches so the
        // migration creates them.
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
        FieldDefinition::builder("alt", FieldType::Text).build(),
        caption,
    ];
    def
}

/// Regression: the per-document upload serve gate runs a visibility find. For an
/// upload collection with a localized field, the find's SELECT needs a locale
/// context — without one it referenced the bare logical column (`caption`
/// instead of `caption__en`), the query errored, and *every* file in the
/// collection 404'd (e.g. broken thumbnails). The gate now passes the default
/// locale.
#[tokio::test]
async fn serve_upload_localized_collection_resolves_owning_doc() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.locale = make_locale_config(); // en + de → localized columns are per-locale
    let app = setup_app_with_config(vec![make_localized_upload_def()], vec![], config);

    // A document that owns the file (matched by `url`), plus the file on disk.
    {
        let conn = app.pool.get().unwrap();
        conn.execute(
            "INSERT INTO media (id, filename, url, caption__en) \
             VALUES ('m1', 'pic.png', '/uploads/media/pic.png', 'hi')",
            &[],
        )
        .unwrap();
    }
    let upload_dir = app._tmp.path().join("uploads").join("media");
    fs::create_dir_all(&upload_dir).unwrap();
    fs::write(upload_dir.join("pic.png"), tiny_png()).unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/media/pic.png")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a localized upload collection's file must serve (regression: localized \
         column made the visibility find error → 404)"
    );
}

/// Regression: under `[access] default_deny = true` (the secure default),
/// a collection with no read hook denies reads — so the upload serve route must
/// NOT serve its files via the public fast path. An anonymous request 404s.
#[tokio::test]
async fn serve_upload_respects_default_deny() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.access.default_deny = true;

    let mut def = CollectionDefinition::new("ddmedia");
    def.timestamps = true;
    def.upload = Some(CollectionUpload::new());
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];

    let app = setup_app_with_config(vec![def], vec![], config);

    {
        let conn = app.pool.get().unwrap();
        conn.execute(
            "INSERT INTO ddmedia (id, filename, url, created_at, updated_at) \
               VALUES ('d1', 'secret.png', '/uploads/ddmedia/secret.png', '2026-01-01', '2026-01-01')",
            &[],
        )
        .unwrap();
    }
    let dir = app._tmp.path().join("uploads").join("ddmedia");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("secret.png"), b"x").unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/uploads/ddmedia/secret.png")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "under default_deny a hook-less collection must not serve files"
    );
}

/// Regression: the serve route enforces the document VIEW model, not just
/// collection read. An upload collection with soft-delete is past the public
/// fast-path, so every file is gated by its owning document, resolved via the
/// requested URL. A live doc's file serves; a TRASHED doc's file 404s (the view
/// excludes trashed rows); and a file with NO owning document (orphan) 404s.
/// Trash exclusion is hook-independent, so this exercises the gate without Lua.
#[tokio::test]
async fn serve_upload_gates_by_owning_document() {
    let mut def = CollectionDefinition::new("media");
    def.timestamps = true;
    def.soft_delete = true;
    def.upload = Some(CollectionUpload::new());
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];

    let app = setup_app(vec![def], vec![]);

    {
        let conn = app.pool.get().unwrap();
        conn.execute_batch(
            "INSERT INTO media (id, filename, url, created_at, updated_at) \
               VALUES ('d1', 'live.png', '/uploads/media/live.png', '2026-01-01', '2026-01-01');\
             INSERT INTO media (id, filename, url, _deleted_at, created_at, updated_at) \
               VALUES ('d2', 'trash.png', '/uploads/media/trash.png', \
                       '2026-01-02', '2026-01-01', '2026-01-01');",
        )
        .unwrap();
    }

    let dir = app._tmp.path().join("uploads").join("media");
    fs::create_dir_all(&dir).unwrap();
    for f in ["live.png", "trash.png", "orphan.png"] {
        fs::write(dir.join(f), b"x").unwrap();
    }

    let serve = |path: &str| {
        app.router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
    };

    // Live document's file → served.
    assert_eq!(
        serve("/uploads/media/live.png").await.unwrap().status(),
        StatusCode::OK,
        "live document's file should serve"
    );
    // Trashed document's file → 404 (the view excludes trashed rows).
    assert_eq!(
        serve("/uploads/media/trash.png").await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "trashed document's file must not serve"
    );
    // File with no owning document (orphan) → 404.
    assert_eq!(
        serve("/uploads/media/orphan.png").await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "orphan file with no owning document must not serve"
    );
}

/// Regression: a row-level read constraint (not just status/trash) flows
/// through the serve gate. A file owned by user A serves to A but 404s for an
/// anonymous request and for user B — the owner filter is combined into the
/// owning-document lookup exactly as it is for a normal read.
#[tokio::test]
async fn serve_upload_gates_by_row_constraint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let access_dir = tmp.path().join("access");
    fs::create_dir_all(&access_dir).unwrap();
    fs::write(
        access_dir.join("owner_only.lua"),
        r"
-- Anonymous denied; authenticated users constrained to their own rows.
return function(ctx)
    if ctx.user == nil then return false end
    return { owner_id = ctx.user.id }
end
",
    )
    .unwrap();

    let mut def = CollectionDefinition::new("omedia");
    def.timestamps = true;
    def.upload = Some(CollectionUpload::new());
    def.access.read = Some(HookRef::new("access.owner_only"));
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
        FieldDefinition::builder("owner_id", FieldType::Text).build(),
    ];

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(vec![def, make_users_def()], vec![], config, tmp);

    let owner_id = create_test_user(&app, "owner@test.com", "secret123");
    let other_id = create_test_user(&app, "other@test.com", "secret123");

    {
        let conn = app.pool.get().unwrap();
        conn.execute(
            "INSERT INTO omedia (id, filename, url, owner_id, created_at, updated_at) \
               VALUES ('m1', 'mine.png', '/uploads/omedia/mine.png', ?1, '2026-01-01', '2026-01-01')",
            &[DbValue::Text(owner_id.clone())],
        )
        .unwrap();
    }

    let dir = app._tmp.path().join("uploads").join("omedia");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("mine.png"), b"x").unwrap();

    let serve = |cookie: Option<String>| {
        let mut req = Request::get("/uploads/omedia/mine.png");
        if let Some(c) = cookie {
            req = req.header(COOKIE, c);
        }
        app.router.clone().oneshot(req.body(Body::empty()).unwrap())
    };

    // Anonymous → read denied → 404.
    assert_eq!(
        serve(None).await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "anonymous must not serve an owner-constrained file"
    );

    // The owner → 200.
    let owner_cookie = make_auth_cookie(&app, &owner_id, "owner@test.com");
    assert_eq!(
        serve(Some(owner_cookie)).await.unwrap().status(),
        StatusCode::OK,
        "the owner should serve their own file"
    );

    // A different user → 404 (the constraint excludes their row).
    let other_cookie = make_auth_cookie(&app, &other_id, "other@test.com");
    assert_eq!(
        serve(Some(other_cookie)).await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "a non-owner must not serve the file"
    );

    // A credential that no longer authenticates is served like an anonymous
    // visitor: once the owner's account is locked, their cookie serves nothing.
    let owner_cookie = make_auth_cookie(&app, &owner_id, "owner@test.com");
    {
        let conn = app.pool.get().unwrap();
        query::auth::lock_user(&conn, "users", &owner_id).unwrap();
    }
    assert_eq!(
        serve(Some(owner_cookie)).await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "a locked owner's cookie must not serve the file"
    );
}

/// Regression: the serve gate honors the draft view, not just published.
/// With drafts enabled and draft access denied to anonymous (via the `update`
/// fallback), a published doc's file serves to an anonymous request but a draft
/// doc's file 404s — `include_drafts` downgrades to what the viewer may see.
#[tokio::test]
async fn serve_upload_gates_drafts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let access_dir = tmp.path().join("access");
    fs::create_dir_all(&access_dir).unwrap();
    fs::write(
        access_dir.join("authed_only.lua"),
        r"
-- Only authenticated users may see drafts (draft view falls back to `update`).
return function(ctx)
    return ctx.user ~= nil
end
",
    )
    .unwrap();

    let mut def = CollectionDefinition::new("dmedia");
    def.timestamps = true;
    def.versions = Some(VersionsConfig::new(true, 10));
    def.upload = Some(CollectionUpload::new());
    def.access.update = Some(HookRef::new("access.authed_only"));
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let app = setup_app_in_dir(vec![def], vec![], config, tmp);

    {
        let conn = app.pool.get().unwrap();
        conn.execute_batch(
            "INSERT INTO dmedia (id, filename, url, _status, created_at, updated_at) \
               VALUES ('p1', 'pub.png', '/uploads/dmedia/pub.png', 'published', '2026-01-01', '2026-01-01');\
             INSERT INTO dmedia (id, filename, url, _status, created_at, updated_at) \
               VALUES ('d1', 'draft.png', '/uploads/dmedia/draft.png', 'draft', '2026-01-01', '2026-01-01');",
        )
        .unwrap();
    }

    let dir = app._tmp.path().join("uploads").join("dmedia");
    fs::create_dir_all(&dir).unwrap();
    for f in ["pub.png", "draft.png"] {
        fs::write(dir.join(f), b"x").unwrap();
    }

    let serve = |path: &str| {
        app.router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
    };

    // Published doc's file → served to anonymous (published read is open).
    assert_eq!(
        serve("/uploads/dmedia/pub.png").await.unwrap().status(),
        StatusCode::OK,
        "published file should serve to anonymous"
    );
    // Draft doc's file → 404 (anonymous lacks draft access via the update fallback).
    assert_eq!(
        serve("/uploads/dmedia/draft.png").await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "draft file must not serve to anonymous"
    );
}

#[tokio::test]
async fn upload_path_traversal_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/posts/../../../etc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_path_traversal_in_collection_returns_404() {
    let app = setup_app(vec![make_posts_def()], vec![]);

    let resp = app
        .router
        .oneshot(
            Request::get("/uploads/..%2F..%2Fetc/passwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST,
        "Path traversal should be rejected, got {status}"
    );
}
