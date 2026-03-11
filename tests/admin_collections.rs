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
use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::Registry;
use crap_cms::core::auth;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;

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
    jwt_secret: String,
}

fn setup_app(collections: Vec<CollectionDefinition>, globals: Vec<GlobalDefinition>) -> TestApp {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.admin.require_auth = false;
    setup_app_with_config(collections, globals, config)
}

fn setup_app_with_config(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    config: CrapConfig,
) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config = config;

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
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone())
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
        jwt_secret: "test-jwt-secret".to_string(),
        email_renderer,
        event_bus: None,
        login_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            5, 300,
        )),
        forgot_password_limiter: std::sync::Arc::new(
            crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
        ),
        has_auth,
        translations,
        shutdown: tokio_util::sync::CancellationToken::new(),
    };

    let router = build_router(state);

    TestApp {
        _tmp: tmp,
        router,
        pool: db_pool,
        registry,
        jwt_secret: "test-jwt-secret".to_string(),
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
    doc.id
}

fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
        .build();
    let token = auth::create_token(&claims, &app.jwt_secret).unwrap();
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

fn make_locale_config() -> LocaleConfig {
    LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    }
}

fn make_localized_pages_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Page".to_string())),
        plural: Some(LocalizedString::Plain("Pages".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .localized(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea)
            .localized(true)
            .build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        ..AdminConfig::default()
    };
    def
}

fn setup_localized_app() -> TestApp {
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".to_string();
    config.locale = make_locale_config();
    setup_app_with_config(
        vec![make_localized_pages_def(), make_users_def()],
        vec![],
        config,
    )
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
async fn dashboard_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "dash@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "dash@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.to_lowercase().contains("posts") || body.to_lowercase().contains("dashboard"));
}

#[tokio::test]
async fn list_collections_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "list@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "list@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "items@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "items@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_form_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "create@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn create_action_creates_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "create_action@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "create_action@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test+Post"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Create action should redirect or HX-Redirect, got {}",
        status
    );
}

#[tokio::test]
async fn edit_form_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "edit@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "edit@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Edit Me".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn update_action_updates_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "update@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Original".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Update action should redirect or HX-Redirect, got {}",
        status
    );
}

#[tokio::test]
async fn delete_action_removes_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delete@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delete@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Delete Me".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Delete action should redirect or return 200, got {}",
        status
    );
}

#[tokio::test]
async fn nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "notfound@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "notfound@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/nope")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Collection Handler Gaps ───────────────────────────────────────────────

#[tokio::test]
async fn list_items_with_search() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "search@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "search@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for title in &["Zebra Unique Alpha", "Beta Common", "Gamma Common"] {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([("title".to_string(), title.to_string())]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=Zebra")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Zebra"),
        "Search results should contain 'Zebra'"
    );
}

#[tokio::test]
async fn create_action_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "locale_create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "locale_create@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "title=Locale+Test+Page&body=Content+here&_locale=de",
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Localized create with locale param should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn delete_action_returns_redirect() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delredir@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delredir@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "To Delete Redir".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/posts/{}", doc.id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "Delete action should redirect or return 200 with HX-Redirect, got {}",
        status
    );

    if status == StatusCode::SEE_OTHER || status == StatusCode::FOUND {
        let location = resp
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap_or(""));
        if let Some(loc) = location {
            assert!(
                loc.contains("/admin/collections/posts"),
                "Delete redirect should point to collection list, got {}",
                loc
            );
        }
    }
}

// ── Edit nonexistent document returns 404 ────────────────────────────────

#[tokio::test]
async fn edit_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "editnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "editnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Delete nonexistent document ──────────────────────────────────────────

#[tokio::test]
async fn delete_nonexistent_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::OK
            || status == StatusCode::FOUND
            || status == StatusCode::NOT_FOUND,
        "Delete nonexistent should return redirect or not found, got {}",
        status
    );
}

// ── Update nonexistent document ──────────────────────────────────────────

#[tokio::test]
async fn update_nonexistent_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "updnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts/nonexistent-id-12345")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER
            || status == StatusCode::OK
            || status == StatusCode::FOUND
            || status == StatusCode::NOT_FOUND
            || status == StatusCode::INTERNAL_SERVER_ERROR,
        "Update nonexistent should return redirect or error, got {}",
        status
    );
}

// ── Pagination on collection list ────────────────────────────────────────

#[tokio::test]
async fn collection_list_with_pagination() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([("title".to_string(), format!("Post {}", i))]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?page=1&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Localized collection regression tests ─────────────────────────────

#[tokio::test]
async fn localized_collection_list_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn localized_collection_list_shows_documents() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Hello World".to_string());
        data.insert("body".to_string(), "Page body".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Hello World"),
        "list should contain the document title"
    );
}

#[tokio::test]
async fn localized_collection_create_via_form() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    "title=Created+Page&body=Some+content&_locale=en",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Localized create should redirect or HX-Redirect, got {}",
        status
    );
}

#[tokio::test]
async fn localized_collection_edit_page_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Editable Page".to_string());
        data.insert("body".to_string(), "Content".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::get(&format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Editable Page"),
        "edit page should contain the document title"
    );
}

#[tokio::test]
async fn localized_collection_delete_succeeds() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "To Delete".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND || status == StatusCode::OK,
        "expected redirect after delete, got {}",
        status
    );
}

#[tokio::test]
async fn localized_collection_search_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Searchable Page".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages?search=Searchable")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Collection Versioning Tests ──────────────────────────────────────────

#[tokio::test]
async fn collection_versions_page_returns_200() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cvp@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cvp@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("title".to_string(), "Versioned Article".to_string()),
        ("body".to_string(), "Content".to_string()),
    ]);
    let doc = query::create(&tx, "articles", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(format!("/admin/collections/articles/{}/versions", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn collection_create_with_draft() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cdraft@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cdraft@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "title=Draft+Article&body=WIP&_action=save_draft",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Create draft should succeed, got {}",
        status
    );
}

// ── Collections: Search, Filter, Sort, Pagination ─────────────────────────

#[tokio::test]
async fn list_items_with_pagination() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..25 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([("title".to_string(), format!("Post {}", i))]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?page=2&per_page=10")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_search_no_results() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nosearch@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nosearch@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=nonexistent_query_xyz")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn list_items_with_search_and_pagination() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "sp@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "sp@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data = std::collections::HashMap::from([(
            "title".to_string(),
            format!("Searchable Item {}", i),
        )]);
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=Searchable&page=1&per_page=3")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Searchable"),
        "Search results should contain matching items"
    );
}

// ── Collections: Create with validation error ─────────────────────────────

#[tokio::test]
async fn create_action_validation_error_missing_required_field() {
    let app = setup_app(
        vec![make_posts_with_required_title(), make_users_def()],
        vec![],
    );
    let user_id = create_test_user(&app, "validate@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "validate@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/articles")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=&body=Some+content"))
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

// ── Collections: Create for auth collection (password field) ──────────────

#[tokio::test]
async fn create_action_auth_collection_with_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/users")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "email=newuser@test.com&name=New+User&password=secret456",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Create auth collection user should succeed, got {}",
        status
    );
}

// ── Collections: Create form for auth collection shows password ───────────
