//! Integration tests for the `/uploads/{collection}/{filename}` serve gate:
//! which viewer may fetch which file, through the full router.

#![allow(clippy::missing_panics_doc, clippy::too_many_lines)]

use std::sync::{Arc, atomic::AtomicUsize};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crap_cms::{
    admin::{
        AdminState, custom_pages::CustomPageRegistry, server::build_router, templates,
        test_support::test_infra, translations::Translations,
    },
    config::{CrapConfig, EmailConfig, UploadConfig},
    core::{
        HookRef, Registry, SharedTokenProvider,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        collection::{CollectionDefinition, VersionsConfig},
        email::create_email_provider,
        field::{FieldDefinition, FieldType},
        rate_limit::LoginRateLimiter,
        upload::{CollectionUpload, create_storage},
    },
    db::{DbConnection, DbPool, migrate, pool, query},
    hooks::lifecycle::HookRunner,
};

const SECRET: &str = "test-jwt-secret";

struct TestApp {
    tmp: TempDir,
    router: Router,
    pool: DbPool,
}

/// Build the full router over `defs`, with the Lua `files` (path, source)
/// written into the config dir before the hook runner loads them.
fn setup(defs: Vec<CollectionDefinition>, files: &[(&str, &str)]) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

    for (path, source) in files {
        let path = tmp.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, source).unwrap();
    }

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = SECRET.into();
    config.admin.require_auth = false;
    config.access.default_deny = false;

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in defs {
            reg.register_collection(def);
        }
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("handlebars");

    let storage = create_storage(tmp.path(), &UploadConfig::default()).unwrap();
    let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new(SECRET));
    let infra = test_infra(
        db_pool.clone(),
        Arc::clone(&registry),
        hook_runner,
        storage,
        token_provider,
        &config,
        tmp.path(),
    );

    let state = AdminState {
        mcp_sessions: Arc::default(),
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        handlebars,
        jwt_secret: SECRET.into(),
        email_provider: create_email_provider(&EmailConfig::default()).unwrap(),
        login_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_login_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: Arc::new(LoginRateLimiter::new(3, 900)),
        ip_forgot_password_limiter: Arc::new(LoginRateLimiter::new(20, 900)),
        mfa_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        has_auth: false,
        translations,
        sse_connections: Arc::new(AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: CancellationToken::new(),
        password_provider: Arc::new(Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: CustomPageRegistry::default(),
    };

    TestApp {
        router: build_router(state),
        tmp,
        pool: db_pool,
    }
}

/// Put `names` on disk under the collection's upload directory.
fn write_files(app: &TestApp, slug: &str, names: &[&str]) {
    let dir = app.tmp.path().join("uploads").join(slug);
    std::fs::create_dir_all(&dir).unwrap();

    for name in names {
        std::fs::write(dir.join(name), b"file-bytes").unwrap();
    }
}

async fn status(app: &TestApp, uri: &str) -> StatusCode {
    app.router
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

/// A drafts-enabled upload collection with the injected file columns.
fn drafted_media(slug: &str) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.timestamps = true;
    def.versions = Some(VersionsConfig::new(true, 10));
    def.upload = Some(CollectionUpload::new());
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];
    def
}

/// A published document whose row names `old.png`, with a pending draft that
/// replaced the file with `new.png` — named by the draft's snapshot only.
fn publish_with_drafted_replacement(app: &TestApp, slug: &str) {
    let conn = app.pool.get().unwrap();

    conn.execute_batch(&format!(
        "INSERT INTO {slug} (id, filename, url, _status, created_at, updated_at) \
           VALUES ('p1', 'old.png', '/uploads/{slug}/old.png', 'published', \
                   '2026-01-01', '2026-01-01');"
    ))
    .unwrap();

    let published = json!({ "filename": "old.png", "url": format!("/uploads/{slug}/old.png") });
    let drafted = json!({ "filename": "new.png", "url": format!("/uploads/{slug}/new.png") });

    query::create_version(&conn, slug, "p1", "published", &published).unwrap();
    query::create_version(&conn, slug, "p1", "draft", &drafted).unwrap();

    write_files(app, slug, &["old.png", "new.png"]);
}

/// Regression: a pending draft's replacement file lives only in the draft's
/// version snapshot, and the serve gate matched files against rows only — so
/// the edit form's preview of a drafted file 404'd for the very editor who
/// uploaded it. The drafted file now serves to a viewer whose draft view shows
/// that draft.
#[tokio::test]
async fn a_drafted_replacement_file_serves_to_a_viewer_with_draft_access() {
    let app = setup(vec![drafted_media("dmedia")], &[]);
    publish_with_drafted_replacement(&app, "dmedia");

    assert_eq!(
        status(&app, "/uploads/dmedia/new.png").await,
        StatusCode::OK,
        "the drafted file serves to a viewer with draft access"
    );
    assert_eq!(
        status(&app, "/uploads/dmedia/old.png").await,
        StatusCode::OK,
        "the published file still serves"
    );
}

/// The drafted file is exactly as private as the draft: a viewer who may read
/// the published document but not its drafts gets 404 for the drafted file,
/// and the published file keeps serving to them.
#[tokio::test]
async fn a_drafted_replacement_file_is_hidden_without_draft_access() {
    let mut def = drafted_media("lmedia");
    def.access.update = Some(HookRef::new("access.deny"));

    let app = setup(
        vec![def],
        &[(
            "access/deny.lua",
            "return function(ctx)\n    return false\nend\n",
        )],
    );
    publish_with_drafted_replacement(&app, "lmedia");

    assert_eq!(
        status(&app, "/uploads/lmedia/new.png").await,
        StatusCode::NOT_FOUND,
        "the drafted file must not serve without draft access"
    );
    assert_eq!(
        status(&app, "/uploads/lmedia/old.png").await,
        StatusCode::OK,
        "the published file serves to a published-only reader"
    );
}

/// Regression: the provably-public fast path skipped `before_read` hooks, so a
/// file whose read a hook aborts on every other surface was served — with an
/// `immutable` public cache policy.
#[tokio::test]
async fn a_before_read_hook_that_aborts_blocks_the_file() {
    let mut def = CollectionDefinition::new("hmedia");
    def.upload = Some(CollectionUpload::new());
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];
    def.hooks.before_read = vec![HookRef::new("hooks.block")];

    let app = setup(
        vec![def],
        &[(
            "hooks/block.lua",
            "return function(ctx)\n    error(\"reads are blocked\")\nend\n",
        )],
    );

    {
        let conn = app.pool.get().unwrap();
        conn.execute_batch(
            "INSERT INTO hmedia (id, filename, url, created_at, updated_at) \
               VALUES ('h1', 'a.png', '/uploads/hmedia/a.png', '2026-01-01', '2026-01-01');",
        )
        .unwrap();
    }
    write_files(&app, "hmedia", &["a.png"]);

    assert_eq!(
        status(&app, "/uploads/hmedia/a.png").await,
        StatusCode::NOT_FOUND
    );
}
