//! Collection-related integration tests for admin HTTP handlers.
//!
//! Covers: collection CRUD, search/filter/sort, validation, versioning, uploads (API).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crap_cms::admin::AdminState;
use crap_cms::admin::server::build_router;
use crap_cms::admin::templates;
use crap_cms::admin::translations::Translations;
use crap_cms::config::CrapConfig;
use crap_cms::core::auth;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::core::{JwtSecret, Registry};
use crap_cms::db::{DbConnection, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────────

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
        FieldDefinition::builder("name", FieldType::Text).build(),
    ];
    def.auth = Some(Auth {
        enabled: true,
        ..Default::default()
    });
    def
}

struct TestApp {
    _tmp: tempfile::TempDir,
    router: axum::Router,
    pool: crap_cms::db::DbPool,
    registry: crap_cms::core::SharedRegistry,
    jwt_secret: JwtSecret,
}

fn setup_app(collections: Vec<CollectionDefinition>, globals: Vec<GlobalDefinition>) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    setup_app_with_config(collections, globals, config)
}

fn setup_app_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        for def in &collections {
            reg.register_collection(def.clone());
        }
        for def in &globals {
            reg.register_global(def.clone());
        }
    }

    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("create hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("create handlebars");
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let has_auth = {
        let reg = registry.read().unwrap();
        reg.collections.values().any(|d| d.is_auth_collection())
    };

    let state = AdminState {
        config,
        config_dir: tmp.path().to_path_buf(),
        pool: db_pool.clone(),
        registry: Registry::snapshot(&registry),
        handlebars,
        hook_runner,
        jwt_secret: "test-jwt-secret".into(),
        email_renderer,
        email_provider: crap_cms::core::email::create_email_provider(
            &crap_cms::config::EmailConfig::default(),
        )
        .unwrap(),
        event_transport: None,
        login_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            5, 300,
        )),
        ip_login_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 300,
        )),
        forgot_password_limiter: std::sync::Arc::new(
            crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        ip_forgot_password_limiter: std::sync::Arc::new(
            crap_cms::core::rate_limit::LoginRateLimiter::new(20, 900),
        ),
        has_auth,
        translations,
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
        storage: crap_cms::core::upload::create_storage(
            tmp.path(),
            &crap_cms::config::UploadConfig::default(),
        )
        .unwrap(),
        token_provider: std::sync::Arc::new(crap_cms::core::auth::JwtTokenProvider::new(
            "test-secret",
        )),
        password_provider: std::sync::Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        invalidation_transport: std::sync::Arc::new(
            crap_cms::core::event::InProcessInvalidationBus::new(),
        ),
        populate_singleflight: std::sync::Arc::new(crap_cms::db::query::Singleflight::new()),
        cache: None,
        custom_pages: Default::default(),
    };

    let router = build_router(state);

    TestApp {
        _tmp: tmp,
        router,
        pool: db_pool,
        registry,
        jwt_secret: "test-jwt-secret".into(),
    }
}

fn create_test_user(app: &TestApp, email: &str, password: &str) -> String {
    let reg = app.registry.read().unwrap();
    let def = reg.get_collection("users").unwrap().clone();
    drop(reg);

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("email".to_string(), email.to_string()),
        ("name".to_string(), "Test User".to_string()),
    ]);
    let doc = query::create(&tx, "users", &def, &data, None).unwrap();
    query::update_password(&tx, "users", &doc.id, password).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("crap_session={}", token)
}

const TEST_CSRF: &str = "test-csrf-token-12345";

fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{}; crap_csrf={}", auth_cookie, TEST_CSRF)
}

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn make_versioned_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.versions = Some(VersionsConfig::new(true, 10));
    def
}

fn make_posts_with_required_title() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def
}

// ── 1C. Dashboard & Collections ───────────────────────────────────────────

#[tokio::test]
async fn create_form_auth_collection_shows_password_field() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/users/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("password"),
        "Auth collection create form should contain password field"
    );
}

// ── Collections: Edit form for auth collection shows password ─────────────

#[tokio::test]
async fn edit_form_auth_collection_shows_password_field() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/users/{}", user_id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("password"),
        "Auth collection edit form should contain password field"
    );
    assert!(
        body.contains("Leave blank")
            || body.contains("leave blank")
            || body.contains("keep current"),
        "Edit form should indicate password can be left blank"
    );
}

// ── Collections: Update action with validation error ──────────────────────

#[tokio::test]
async fn update_action_validation_error() {
    let app = setup_app(
        vec![make_posts_with_required_title(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "update_val@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "update_val@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Valid Title".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/articles/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=&body=Updated+content"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Expected 200 (validation error re-render) or redirect, got {}",
        status
    );
}

#[tokio::test]
async fn delete_confirm_nonexistent_doc_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Collections: Delete via POST with _method=DELETE ──────────────────────

#[tokio::test]
async fn update_action_post_with_method_delete() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "meth_del@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "meth_del@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "Method Delete Test".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("_method=DELETE"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "POST with _method=DELETE should succeed, got {}",
        status
    );
}

// ── Collections: Edit nonexistent document ────────────────────────────────

#[tokio::test]
async fn edit_form_nonexistent_doc_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "editnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "editnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Collections: Versioning ───────────────────────────────────────────────

#[tokio::test]
async fn versioned_collection_list_returns_200() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/articles")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn versioned_collection_create_as_draft() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "draft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "draft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Draft+Post&_action=save_draft"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Create as draft should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn versioned_collection_edit_shows_versions() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "editver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "editver@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "Versioned Doc".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version") || body.contains("versions"),
        "Edit page for versioned collection should show version info"
    );
}

#[tokio::test]
async fn versioned_collection_update_unpublish() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "unpub@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "unpub@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "Published Post".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/articles/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Published+Post&_action=unpublish"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Unpublish should succeed, got {}",
        status
    );
}

/// Regression: unpublish on a versioned collection whose `title` field is
/// `localized = true` and locales are enabled (`["en", "de"]`) used to fail
/// silently. The handler called `service::unpublish_document(&ctx, id)`,
/// which routed through `find_by_id_raw(... locale_ctx: None ...)`. With
/// `None`, the SELECT generator falls back to bare column names (`title`)
/// instead of locale-suffixed ones (`title__en`, `title__de`) — but the
/// table only has the suffixed columns, so SQLite returned `no such
/// column: title`. The error was caught by the catch-all match arm in
/// `do_update`, logged, and the user redirected to the same edit page —
/// "unpublish button does nothing" from the user's perspective.
///
/// Fix: thread the locale config through `ServiceContext` so the unpublish
/// path can build a default `LocaleContext` (`Mode::All`) for the raw read.
#[tokio::test]
async fn versioned_collection_unpublish_with_localized_field() {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;
    config.locale = crap_cms::config::LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };

    let mut def = CollectionDefinition::new("articles");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Article".to_string())),
        plural: Some(LocalizedString::Plain("Articles".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .localized(true)
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def.versions = Some(VersionsConfig::new(true, 10));

    let app = setup_app_with_config(vec![def, make_users_def()], vec![], config);
    let user_id = create_test_user(&app, "loc-unpub@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "loc-unpub@test.com");

    // Seed a published row directly. Localized columns store per-locale
    // values (`title__en`, `title__de`); we set both so the row is valid.
    let conn = app.pool.get().unwrap();
    let id = "loc-doc-1";
    conn.execute(
        "INSERT INTO articles (id, title__en, title__de, _status, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'published', '2026-01-01', '2026-01-01')",
        &[
            crap_cms::db::DbValue::Text(id.into()),
            crap_cms::db::DbValue::Text("Hello".into()),
            crap_cms::db::DbValue::Text("Hallo".into()),
        ],
    )
    .unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/articles/{id}"))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Hello&_action=unpublish&_locale=en"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Localized unpublish should succeed, got {}",
        status
    );

    // Confirm the document was actually flipped to draft, not just that the
    // request returned 303 (the buggy path also returned 303 — silently —
    // because the catch-all error arm redirected without doing the update).
    let conn = app.pool.get().unwrap();
    let row = conn
        .query_one(
            "SELECT _status FROM articles WHERE id = ?1",
            &[crap_cms::db::DbValue::Text(id.into())],
        )
        .unwrap()
        .expect("row exists");
    let status_value = match row.get_value(0) {
        Some(crap_cms::db::DbValue::Text(s)) => s.clone(),
        other => panic!("expected text _status, got {other:?}"),
    };
    assert_eq!(
        status_value, "draft",
        "unpublish must flip _status to 'draft' when fields are localized"
    );

    // Regression: the version snapshot saved by `persist_unpublish` must use
    // the same flat-key shape as every other write path. An earlier fix
    // attempt used `LocaleMode::All`, which triggered `group_locale_fields`
    // and produced `title: {"en": "Hello", "de": "Hallo"}` — a shape that
    // diverged from `persist_draft_version` snapshots, broke user hooks
    // expecting flat keys, and surfaced as malformed data in broadcast
    // events and the version sidebar.
    let snapshot_row = conn
        .query_one(
            "SELECT snapshot FROM _versions_articles WHERE _parent = ?1 ORDER BY _version DESC LIMIT 1",
            &[crap_cms::db::DbValue::Text(id.into())],
        )
        .unwrap()
        .expect("unpublish snapshot exists");
    let snapshot_text = match snapshot_row.get_value(0) {
        Some(crap_cms::db::DbValue::Text(s)) => s.clone(),
        other => panic!("expected text snapshot, got {other:?}"),
    };
    let snapshot: serde_json::Value = serde_json::from_str(&snapshot_text).unwrap();
    let title = snapshot.get("title").expect("snapshot has title");
    assert!(
        title.is_string(),
        "snapshot title must be a flat string (resolved at default locale), \
         not a grouped object — got {title:?}",
    );
    assert_eq!(title.as_str(), Some("Hello"));
}

#[tokio::test]
async fn versioned_collection_versions_page() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "verpage@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "verpage@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "Versioned Page".to_string())]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}/versions", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.to_lowercase().contains("version"),
        "Versions page should contain 'version'"
    );
}

#[tokio::test]
async fn non_versioned_collection_versions_page_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nover@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nover@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "No Versions".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}/versions", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::FOUND
            || status == StatusCode::TEMPORARY_REDIRECT,
        "Non-versioned collection versions page should redirect, got {}",
        status
    );
}

#[tokio::test]
async fn restore_version_non_versioned_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnv@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "No Versions".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!(
                "/admin/collections/posts/{}/versions/fake-ver/restore",
                doc.id
            ))
            .header("cookie", auth_and_csrf(&cookie))
            .header("X-CSRF-Token", TEST_CSRF)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Restore version on non-versioned collection should redirect, got {}",
        status
    );
}

// ── Collections: Evaluate conditions endpoint ─────────────────────────────

#[tokio::test]
async fn evaluate_conditions_returns_json() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cond@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cond@test.com");

    let body_json = json!({
        "form_data": {"title": "Test"},
        "conditions": {}
    });

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts/evaluate-conditions")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body_json).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        json.is_object(),
        "Evaluate conditions should return a JSON object"
    );
}

// ── Collections: list_searchable_fields configuration ─────────────────────
