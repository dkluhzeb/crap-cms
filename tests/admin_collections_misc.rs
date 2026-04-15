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
use crap_cms::core::auth;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::core::{JwtSecret, Registry};
use crap_cms::db::{DbConnection, migrate, pool, query};
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
        csp_header: None,
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

fn csrf_cookie() -> String {
    format!("crap_csrf={}", TEST_CSRF)
}

fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{}; crap_csrf={}", auth_cookie, TEST_CSRF)
}

async fn body_string(body: Body) -> String {
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn make_bearer_token(app: &TestApp, user_id: &str, email: &str) -> String {
    let claims = auth::Claims::builder(user_id, "users")
        .email(email)
        .exp((chrono::Utc::now().timestamp() as u64) + 3600)
        .build()
        .unwrap();
    let token = auth::create_token(&claims, app.jwt_secret.as_ref()).unwrap();
    format!("Bearer {}", token)
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
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
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

fn make_searchable_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("sposts");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Searchable Post".to_string())),
        plural: Some(LocalizedString::Plain("Searchable Posts".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
        FieldDefinition::builder("category", FieldType::Text).build(),
    ];
    def.admin = AdminConfig {
        use_as_title: Some("title".to_string()),
        list_searchable_fields: vec!["title".to_string(), "body".to_string()],
        ..AdminConfig::default()
    };
    def
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

    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"_file\"; filename=\"{}\"\r\n",
            filename
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {}\r\n\r\n", content_type).as_bytes());
    body.extend_from_slice(file_data);
    body.extend_from_slice(b"\r\n");

    for (name, value) in fields {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", name).as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let content_type = format!("multipart/form-data; boundary={}", boundary);
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

// ── 1C. Dashboard & Collections ───────────────────────────────────────────

#[tokio::test]
async fn search_uses_configured_searchable_fields() {
    let app = setup_app(vec![make_searchable_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "search2@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "search2@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("sposts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([
        ("title".to_string(), "Unique Title XYZ".to_string()),
        ("body".to_string(), "Some body text".to_string()),
        ("category".to_string(), "tech".to_string()),
    ]);
    let doc = query::create(&tx, "sposts", &def, &data, None).unwrap();
    query::fts::fts_upsert(&tx, "sposts", &doc, Some(&def)).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/sposts?search=Unique")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("Unique Title XYZ"),
        "Search should find by configured searchable fields"
    );
}

// ── Collections: Update via locale redirect suffix ────────────────────────

#[tokio::test]
async fn update_localized_collection_redirects_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "updloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updloc@test.com");

    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);
        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Update Locale".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated+Title&_locale=de"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Localized update should succeed, got {}",
        status
    );
    if status == StatusCode::OK
        && let Some(hx_redir) = resp.headers().get("HX-Redirect")
    {
        let redir = hx_redir.to_str().unwrap_or("");
        assert!(
            !redir.contains("locale="),
            "HX-Redirect should not contain locale= (cookie-based now), got {}",
            redir
        );
    }
}

// ── Collections: Nonexistent collection create form ───────────────────────

#[tokio::test]
async fn create_form_nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "cfnf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfnf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/nonexistent/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── Collections: Create action nonexistent collection ─────────────────────

#[tokio::test]
async fn create_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "canf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "canf@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/nonexistent")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::FOUND,
        "Create on nonexistent collection should redirect, got {}",
        status
    );
}

// ── Collections: Collection with use_as_title in list view ────────────────

#[tokio::test]
async fn list_items_uses_title_field() {
    let mut def = make_posts_def();
    def.admin.use_as_title = Some("title".to_string());

    let app = setup_app(vec![def.clone(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "titlefield@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "titlefield@test.com");

    let real_def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "My Custom Title".to_string())]);
    query::create(&tx, "posts", &real_def, &data, None).unwrap();
    tx.commit().unwrap();

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
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("My Custom Title"),
        "List should show document title via use_as_title"
    );
}

#[tokio::test]
async fn create_action_missing_required_field_shows_errors() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "valerr@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "valerr@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/posts")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title="))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::SEE_OTHER,
        "Validation error should re-render form or redirect, got {}",
        status
    );
}

#[tokio::test]
async fn create_form_auth_collection_includes_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "authform@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "authform@test.com");

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
        "Auth collection create form should have password field"
    );
}

#[tokio::test]
async fn edit_form_auth_collection_includes_password() {
    let app = setup_app(vec![make_users_def()], vec![]);
    let user_id = create_test_user(&app, "authedit@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "authedit@test.com");

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
        "Auth collection edit form should have password field"
    );
}

#[tokio::test]
async fn edit_form_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "nondoc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "nondoc@test.com");

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

#[tokio::test]
async fn delete_confirm_nonexistent_document_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delconfnon@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delconfnon@test.com");

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

#[tokio::test]
async fn versioned_collection_create_form() {
    let app = setup_app(vec![make_versioned_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "ver@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "ver@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/articles/create")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn post_with_method_delete_deletes_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "methoddel@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "methoddel@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "Method Delete".to_string())]);
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
        "DELETE via _method should succeed, got {}",
        status
    );
}

#[tokio::test]
async fn update_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncolu@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncolu@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/nonexistent/someid")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Test"))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Update on nonexistent collection should redirect"
    );
}

#[tokio::test]
async fn delete_action_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncold@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncold@test.com");

    let resp = app
        .router
        .oneshot(
            Request::delete("/admin/collections/nonexistent/someid")
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
        "Delete on nonexistent collection should redirect, got {}",
        status
    );
}

#[tokio::test]
async fn delete_confirm_nonexistent_collection_returns_404() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "noncoldc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "noncoldc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/nonexistent/someid/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn restore_version_nonversioned_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnv@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnv@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "NV Restore".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!(
                "/admin/collections/posts/{}/versions/fake-version/restore",
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
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Restore on non-versioned should redirect"
    );
}

#[tokio::test]
async fn restore_version_nonexistent_collection_redirects() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "restnc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "restnc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/nonexistent/someid/versions/v1/restore")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "Restore on nonexistent collection should redirect"
    );
}

// ── Collections: Create form with locale param ────────────────────────────

#[tokio::test]
async fn create_form_with_locale_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "cfloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages/create")
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Edit form with non-default locale ────────────────────────────────────

#[tokio::test]
async fn edit_form_with_non_default_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "efloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "efloc@test.com");

    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Locale Edit Test".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Update action with locale parameter ──────────────────────────────────

#[tokio::test]
async fn update_action_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "updloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "updloc@test.com");

    let doc_id = {
        let reg = app.registry.read().unwrap();
        let def = reg.get_collection("pages").unwrap().clone();
        drop(reg);

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = std::collections::HashMap::new();
        data.insert("title".to_string(), "Update Locale Test".to_string());
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{}", doc_id))
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("title=Updated+DE&_locale=de".to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "Update with locale should succeed, got {}",
        status
    );
}

// ── Collections: Search with special characters ───────────────────────────

#[tokio::test]
async fn list_items_search_with_special_chars() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "special@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "special@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?search=hello%20world%26foo")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Collections: Create form with locale ──────────────────────────────────

#[tokio::test]
async fn create_form_with_locale() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "cfloc@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "cfloc@test.com");

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/pages/create")
                .header("cookie", format!("{}; crap_editor_locale=de", &cookie))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("DE") || body.contains("de"),
        "Should show locale selector with DE"
    );
}

// ── Upload API tests ─────────────────────────────────────────────────────

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
            Request::get(format!("/admin/collections/media/{}", doc_id))
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
        html.contains("data-src=\"/uploads/"),
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
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"alt\"\r\n\r\nsome text\r\n");
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

    let resp = app
        .router
        .oneshot(
            Request::post("/api/upload/media")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={}", boundary),
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
            Request::patch(format!("/api/upload/media/{}", doc_id))
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
            Request::delete(format!("/api/upload/media/{}", doc_id))
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

// ── Delete confirm page ──────────────────────────────────────────────────

#[tokio::test]
async fn delete_confirm_page_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delconf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delconf@test.com");

    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data =
        std::collections::HashMap::from([("title".to_string(), "To Confirm Delete".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}/delete", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Regression: delete confirmation page should still render (200) even when
/// the document's table has a schema mismatch (e.g., missing column), so that
/// users can delete broken/orphaned documents.
#[tokio::test]
async fn delete_confirm_page_with_schema_mismatch_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delsm@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delsm@test.com");

    // Create a document normally
    let mut conn = app.pool.get().unwrap();
    let def = {
        let reg = app.registry.read().unwrap();
        reg.get_collection("posts").unwrap().clone()
    };
    let tx = conn.transaction().unwrap();
    let data = std::collections::HashMap::from([("title".to_string(), "Broken Doc".to_string())]);
    let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
    tx.commit().unwrap();

    // Simulate schema mismatch: rename the title column so SELECT fails
    conn.execute_batch("ALTER TABLE posts RENAME COLUMN title TO title_old;")
        .unwrap();

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/posts/{}/delete", doc.id))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Should still render the delete confirmation page, not 500
    assert_eq!(resp.status(), StatusCode::OK);
}

// ── Back-reference warning on delete confirmation ────────────────────

#[tokio::test]
async fn delete_confirm_shows_back_references_warning() {
    let media = CollectionDefinition::new("media");
    let mut posts = CollectionDefinition::new("posts");
    posts.labels = Labels {
        singular: Some(LocalizedString::Plain("Post".to_string())),
        plural: Some(LocalizedString::Plain("Posts".to_string())),
    };
    posts.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("image", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build(),
    ];
    let app = setup_app(vec![media, posts, make_users_def()], vec![]);

    let user_id = create_test_user(&app, "admin@test.com", "password123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Create a media document and a post referencing it
    let conn = app.pool.get().unwrap();
    conn.execute("INSERT INTO media (id, _ref_count) VALUES ('m1', 1)", &[])
        .unwrap();
    conn.execute(
        "INSERT INTO posts (id, title, image) VALUES ('p1', 'My Post', 'm1')",
        &[],
    )
    .unwrap();
    drop(conn);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/media/m1/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // Should contain the warning card with ref count info
    assert!(body.contains("card--warning"), "Should show warning card");
}

#[tokio::test]
async fn delete_confirm_no_warning_when_unreferenced() {
    let media = CollectionDefinition::new("media");
    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("image", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build(),
    ];
    let app = setup_app(vec![media, posts, make_users_def()], vec![]);

    let user_id = create_test_user(&app, "admin@test.com", "password123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    // Create a media document with no references
    let conn = app.pool.get().unwrap();
    conn.execute("INSERT INTO media (id) VALUES ('m1')", &[])
        .unwrap();
    drop(conn);

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/media/m1/delete")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(
        !body.contains("card--warning"),
        "Should NOT show warning when unreferenced"
    );
}
