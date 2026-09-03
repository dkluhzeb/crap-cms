//! Collection-related integration tests for admin HTTP handlers.
//!
//! Covers: collection CRUD, search/filter/sort, validation, versioning, uploads (API).

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

use serde_json::json;
use std::collections::HashMap;
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
use crap_cms::core::DocumentFields;
use crap_cms::core::auth;
use crap_cms::core::collection::*;
use crap_cms::core::field::*;
use crap_cms::core::{JwtSecret, Registry};
use crap_cms::db::{DbConnection, DbValue, migrate, pool, query};
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
    def.auth = Some(Auth::enabled());
    def
}

struct TestApp {
    _tmp: tempfile::TempDir,
    router: axum::Router,
    pool: crap_cms::db::DbPool,
    registry: Arc<crap_cms::core::Registry>,
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

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in &collections {
            reg.register_collection(def.clone());
        }
        for def in &globals {
            reg.register_global(def.clone());
        }
    }

    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("create hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("create handlebars");

    let has_auth = registry
        .collections
        .values()
        .any(|d| d.is_auth_collection());

    let storage = crap_cms::core::upload::create_storage(
        tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();
    let token_provider: crap_cms::core::SharedTokenProvider = std::sync::Arc::new(
        crap_cms::core::auth::JwtTokenProvider::new("test-jwt-secret"),
    );
    let infra = crap_cms::admin::test_support::test_infra(
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
        jwt_secret: "test-jwt-secret".into(),
        email_provider: crap_cms::core::email::create_email_provider(
            &crap_cms::config::EmailConfig::default(),
        )
        .unwrap(),
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
        mfa_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: std::sync::Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 300,
        )),
        has_auth,
        translations,
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
        password_provider: std::sync::Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: crap_cms::admin::custom_pages::CustomPageRegistry::default(),
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
    let reg = &app.registry;
    let def = reg.get_collection("users").unwrap().clone();

    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("email".to_string(), json!(email)),
        ("name".to_string(), json!("Test User")),
    ])
    .into();
    let doc = query::create(&tx, "users", &def, &data, None).unwrap();
    query::update_password(&tx, "users", &doc.id, password).unwrap();
    tx.commit().unwrap();
    doc.id.to_string()
}

fn make_auth_cookie(app: &TestApp, user_id: &str, email: &str) -> String {
    // `update_password` bumps `_session_version` to 1 the moment a password
    // is set; the evaluator rejects a default-built Claims (session_version
    // = 0) as `Invalid(StaleSession)`. Read the user's current version so
    // the test cookie matches the DB.
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
    format!("crap_session={token}")
}

const TEST_CSRF: &str = "test-csrf-token-12345";

fn auth_and_csrf(auth_cookie: &str) -> String {
    format!("{auth_cookie}; crap_csrf={TEST_CSRF}")
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

/// Regression test for the user-reported "list shows all (published)
/// items when filter is set to draft" symptom. `_status` is a system
/// column (`_*` prefix) so it cannot ride the generic user-filter
/// pipeline (`validate_user_filters` rejects `_*`). The admin list
/// handler extracts `?where[_status][equals]=X` from the raw query
/// via `extract_status_filter` and forwards it as a typed
/// `status_filter` on `FindDocumentsInput` so it bypasses
/// validation and reaches SQL via the trusted post-validation
/// injection path in `build_effective_query`.
///
/// This test asserts:
/// - unfiltered shows both draft and published rows;
/// - `?where[_status][equals]=draft` narrows to drafts only;
/// - `?where[_status][equals]=published` narrows to published only.
#[tokio::test]
async fn list_items_url_status_filter_narrows_drafts_only() {
    use crap_cms::core::collection::VersionsConfig;

    fn posts_with_drafts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.versions = Some(VersionsConfig::new(true, 10));
        def.admin.use_as_title = Some("title".to_string());
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
        ];
        def
    }

    let app = setup_app(vec![posts_with_drafts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "statusf@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "statusf@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    // Both rows start `_status='published'` (column default per
    // `migrate/collection/create.rs:131`). Demote one to `draft` via a
    // direct UPDATE — this is what the service-layer
    // `persist::create_document` does internally when `is_draft = true`,
    // we just skip the service wrapper here to keep the test focused on
    // the filter pipeline.
    let mut data1 = DocumentFields::new();
    data1.insert("title".to_string(), json!("Live Article"));
    query::create(&tx, "posts", &def, &data1, None).expect("publish create ok");

    let mut data2 = DocumentFields::new();
    data2.insert("title".to_string(), json!("Pending Draft"));
    let draft_doc = query::create(&tx, "posts", &def, &data2, None).expect("draft create ok");
    tx.execute(
        "UPDATE posts SET _status = 'draft' WHERE id = ?1",
        &[DbValue::Text(draft_doc.id.to_string())],
    )
    .expect("set _status=draft");
    tx.commit().unwrap();
    drop(conn);

    fn count_table_rows(body: &str) -> usize {
        let Some(start) = body.find("<tbody") else {
            return 0;
        };
        let Some(end) = body[start..].find("</tbody>").map(|i| start + i) else {
            return 0;
        };
        body[start..end].matches("<tr").count()
    }

    // Sanity: unfiltered shows both (admin defaults include_drafts=true).
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "unfiltered admin list should show both draft and published"
    );

    // Filter to drafts only via `?where[_status][equals]=draft`. URL-encoded
    // brackets — what the browser sends from the filter drawer.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5B_status%5D%5Bequals%5D=draft")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "?where[_status][equals]=draft should narrow to 1 draft row"
    );
    assert!(body.contains("Pending Draft"));
    assert!(
        !body.contains("Live Article"),
        "draft filter must NOT include the published doc — typed-param plumbing regression"
    );

    // Symmetric: filter to published only.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5B_status%5D%5Bequals%5D=published")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "?where[_status][equals]=published should narrow to 1 published row"
    );
    assert!(body.contains("Live Article"));
    assert!(!body.contains("Pending Draft"));

    // Empty `where[_status][equals]=` value (the "All" option in the
    // filter drawer) should fall through to showing both rows — the
    // extractor returns None for empty values, the filter UI's
    // `_collectFilters` skips empty-value rows, but both forms must
    // resolve to the unfiltered list.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5B_status%5D%5Bequals%5D=")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "empty where[_status][equals]= (All) should show both draft and published"
    );

    // Multiple `_status` values across an OR-clause (`(_status=draft OR
    // _status=published)`) widen back to "show both" — the extractor
    // collects every value, the service injects `_status IN (...)`.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(
                "/admin/collections/posts?where[or][0][0][_status][equals]=draft\
                 &where[or][0][1][_status][equals]=published",
            )
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "_status IN (draft, published) should show both rows"
    );
    assert!(body.contains("Live Article"));
    assert!(body.contains("Pending Draft"));
}

/// Regression for the same-field IN-merge: `?where[title][equals]=A&where[title][equals]=B`
/// collapses to `WHERE title IN ('A', 'B')` and returns rows matching either.
/// Cross-field OR via the `where[or][G][N][…]` URL form widens to a true OR.
#[tokio::test]
async fn list_items_or_clause_widens_results() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "or-filter@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "or-filter@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    for title in ["Alpha", "Bravo", "Charlie"] {
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!(title));
        query::create(&tx, "posts", &def, &data, None).unwrap();
    }
    tx.commit().unwrap();
    drop(conn);

    fn count_table_rows(body: &str) -> usize {
        let Some(start) = body.find("<tbody") else {
            return 0;
        };
        let Some(end) = body[start..].find("</tbody>").map(|i| start + i) else {
            return 0;
        };
        body[start..end].matches("<tr").count()
    }

    // Same-field IN merge: two `equals` rows on `title` → `title IN ('Alpha', 'Bravo')`.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(
                "/admin/collections/posts?where[title][equals]=Alpha&where[title][equals]=Bravo",
            )
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(count_table_rows(&body), 2, "IN merge returns 2 rows");
    assert!(body.contains("Alpha"));
    assert!(body.contains("Bravo"));
    assert!(!body.contains("Charlie"));

    // Cross-field OR via `where[or][G][N][…]`: title=Alpha OR title=Charlie.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get(
                "/admin/collections/posts?\
                 where[or][0][0][title][equals]=Alpha\
                 &where[or][0][1][title][equals]=Charlie",
            )
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(count_table_rows(&body), 2, "OR clause returns 2 rows");
    assert!(body.contains("Alpha"));
    assert!(body.contains("Charlie"));
    assert!(!body.contains("Bravo"));
}

/// Regression test for the user-reported "filter has no effect" symptom.
/// Both `where[status][equals]=draft` (raw) and the URL-encoded form
/// `where%5Bstatus%5D%5Bequals%5D=draft` (which is what the browser
/// produces when you click Apply) must narrow the list to draft items
/// only.
#[tokio::test]
async fn list_items_url_filter_narrows_results() {
    fn posts_with_status_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.admin.use_as_title = Some("title".to_string());
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("status", FieldType::Select)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Draft".to_string()), "draft"),
                    SelectOption::new(LocalizedString::Plain("Published".to_string()), "published"),
                ])
                .build(),
        ];
        def
    }

    /// Count `<tr>` rows inside the items `<tbody>` of the rendered list page.
    fn count_table_rows(body: &str) -> usize {
        let Some(start) = body.find("<tbody") else {
            return 0;
        };
        let Some(end) = body[start..].find("</tbody>").map(|i| start + i) else {
            return 0;
        };
        body[start..end].matches("<tr").count()
    }

    let app = setup_app(vec![posts_with_status_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "filter@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "filter@test.com");

    // Insert one draft + one published post.
    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let mut data1 = DocumentFields::new();
    data1.insert("title".to_string(), json!("Draft Post"));
    data1.insert("status".to_string(), json!("draft"));
    query::create(&tx, "posts", &def, &data1, None).unwrap();
    let mut data2 = DocumentFields::new();
    data2.insert("title".to_string(), json!("Published Post"));
    data2.insert("status".to_string(), json!("published"));
    query::create(&tx, "posts", &def, &data2, None).unwrap();
    tx.commit().unwrap();
    drop(conn);

    // Sanity: no filter shows both.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        2,
        "unfiltered list should show both posts"
    );
    assert!(
        body.contains("Draft Post"),
        "title 'Draft Post' must appear"
    );
    assert!(
        body.contains("Published Post"),
        "title 'Published Post' must appear"
    );

    // Filter via raw `where[status][equals]=draft`. List should only
    // show the draft post.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where[status][equals]=draft")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "raw-filter list should narrow to 1 row (the draft post)"
    );
    assert!(body.contains("Draft Post"));
    assert!(
        !body.contains("Published Post"),
        "raw-filter list must NOT contain the published post"
    );

    // Filter via URL-encoded `where%5Bstatus%5D%5Bequals%5D=draft` — what
    // the browser actually sends when JS builds the URL.
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?where%5Bstatus%5D%5Bequals%5D=draft")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert_eq!(
        count_table_rows(&body),
        1,
        "encoded-filter list should narrow to 1 row"
    );
    assert!(body.contains("Draft Post"));
    assert!(
        !body.contains("Published Post"),
        "encoded-filter list must NOT contain the published post"
    );
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
        "Create action should redirect or HX-Redirect, got {status}"
    );
}

#[tokio::test]
async fn edit_form_returns_200() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "edit@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "edit@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Edit Me"))]).into();
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
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Original"))]).into();
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
        "Update action should redirect or HX-Redirect, got {status}"
    );
}

#[tokio::test]
async fn delete_action_removes_document() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delete@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delete@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Delete Me"))]).into();
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
        "Delete action should redirect or return 200, got {status}"
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
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for title in &["Zebra Unique Alpha", "Beta Common", "Gamma Common"] {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields = HashMap::from([("title".to_string(), json!(title))]).into();
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

/// Creating a document under a NON-default locale is rejected: a new document
/// establishes its default-locale (canonical) row first — translations come
/// later via update. Creating under the default locale succeeds. (Was
/// `create_action_with_locale`, which asserted the now-retired behavior that a
/// non-default-locale create succeeds and silently wrote shared columns from the
/// wrong locale.)
#[tokio::test]
async fn create_under_non_default_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "locale_create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "locale_create@test.com");

    let create = |locale: &str| {
        app.router.clone().oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(format!(
                    "title=Locale+Test+Page&body=Content+here&_locale={locale}"
                )))
                .unwrap(),
        )
    };

    // Non-default locale → rejected.
    let resp = create("de").await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "creating under a non-default locale must be rejected"
    );

    // Default locale → succeeds.
    let resp = create("en").await.unwrap();
    let status = resp.status();
    assert!(
        status == StatusCode::SEE_OTHER || status == StatusCode::OK,
        "default-locale create should succeed, got {status}"
    );
}

/// Regression: an unknown `_locale` on a form post used to be silently
/// swallowed (`from_locale_string(...).unwrap_or(None)`) into "no locale
/// context", which on a localized collection reads/writes bare columns.
/// It must be rejected with a 422 toast naming the invalid locale.
#[tokio::test]
async fn create_with_unknown_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "badlocale_create@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_create@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("title=Bad+Locale&body=Content&_locale=xx"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let toast = resp
        .headers()
        .get("X-Crap-Toast")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        toast.contains("Invalid locale"),
        "toast should name the invalid locale, got: {toast}"
    );
}

/// Regression twin of `create_with_unknown_locale_is_rejected` for the
/// update action.
#[tokio::test]
async fn update_with_unknown_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "badlocale_update@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_update@test.com");

    let def = app.registry.get_collection("pages").unwrap().clone();
    let locale_ctx =
        query::LocaleContext::from_locale_string(Some("en"), &make_locale_config()).unwrap();
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([("title".to_string(), json!("Original"))]).into();
    let doc = query::create(&tx, "pages", &def, &data, locale_ctx.as_ref()).unwrap();
    tx.commit().unwrap();

    let resp = app
        .router
        .oneshot(
            Request::post(format!("/admin/collections/pages/{}", doc.id))
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from("title=Updated&_locale=xx"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let toast = resp
        .headers()
        .get("X-Crap-Toast")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        toast.contains("Invalid locale"),
        "toast should name the invalid locale, got: {toast}"
    );
}

/// Regression twin for the validate endpoint: unknown locale in the JSON
/// payload must produce a validation error, not a silent bare-column run.
#[tokio::test]
async fn validate_with_unknown_locale_is_rejected() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "badlocale_validate@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "badlocale_validate@test.com");

    let resp = app
        .router
        .oneshot(
            Request::post("/admin/collections/pages/validate")
                .header("content-type", "application/json")
                .header("cookie", auth_and_csrf(&cookie))
                .header("X-CSRF-Token", TEST_CSRF)
                .body(Body::from(
                    json!({ "data": { "title": "T" }, "locale": "xx" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_string(resp.into_body()).await;
    assert!(
        body.contains("\"valid\":false") && body.contains("Invalid locale"),
        "validate must reject the unknown locale, got: {body}"
    );
}

#[tokio::test]
async fn delete_action_returns_redirect() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "delredir@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "delredir@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields =
        HashMap::from([("title".to_string(), json!("To Delete Redir"))]).into();
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
        "Delete action should redirect or return 200 with HX-Redirect, got {status}"
    );

    if status == StatusCode::SEE_OTHER || status == StatusCode::FOUND {
        let location = resp
            .headers()
            .get("location")
            .map(|v| v.to_str().unwrap_or(""));
        if let Some(loc) = location {
            assert!(
                loc.contains("/admin/collections/posts"),
                "Delete redirect should point to collection list, got {loc}"
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
        "Delete nonexistent should return redirect or not found, got {status}"
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
        "Update nonexistent should return redirect or error, got {status}"
    );
}

// ── Pagination on collection list ────────────────────────────────────────

#[tokio::test]
async fn collection_list_pagination_multi_page_shows_nav() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Post {}", i)))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    // Page 1 of 3 → has "Next", no "Previous", shows "Page 1 of 3"
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?page=1&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 1 of 3"), "should show page info");
    assert!(body.contains("Next"), "page 1 should have Next link");
    assert!(
        !body.contains("Previous"),
        "page 1 should not have Previous link"
    );

    // Page 2 of 3 → has both "Previous" and "Next"
    let resp = app
        .router
        .clone()
        .oneshot(
            Request::get("/admin/collections/posts?page=2&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 2 of 3"), "should show page info");
    assert!(body.contains("Next"), "page 2 should have Next link");
    assert!(
        body.contains("Previous"),
        "page 2 should have Previous link"
    );

    // Page 3 of 3 → has "Previous", no "Next"
    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?page=3&per_page=2")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 3 of 3"), "should show page info");
    assert!(
        !body.contains("Next"),
        "last page should not have Next link"
    );
    assert!(
        body.contains("Previous"),
        "last page should have Previous link"
    );
}

#[tokio::test]
async fn collection_list_pagination_single_page_no_nav() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "single@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "single@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..3 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Post {}", i)))]).into();
        query::create(&tx, "posts", &def, &data, None).unwrap();
        tx.commit().unwrap();
    }

    let resp = app
        .router
        .oneshot(
            Request::get("/admin/collections/posts?page=1&per_page=10")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp.into_body()).await;
    // 3 docs fit in 1 page of 10 → no navigation links
    assert!(
        !body.contains("Previous"),
        "single page should not have Previous"
    );
    assert!(!body.contains("Next"), "single page should not have Next");
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
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello World"));
        data.insert("body".to_string(), json!("Page body"));
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
        "Localized create should redirect or HX-Redirect, got {status}"
    );
}

#[tokio::test]
async fn localized_collection_edit_page_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    let doc_id = {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Editable Page"));
        data.insert("body".to_string(), json!("Content"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::get(format!("/admin/collections/pages/{doc_id}"))
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
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("To Delete"));
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let doc = query::create(&tx, "pages", &def, &data, Some(&locale_ctx)).unwrap();
        tx.commit().unwrap();
        doc.id
    };

    let resp = app
        .router
        .oneshot(
            Request::delete(format!("/admin/collections/pages/{doc_id}"))
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
        "expected redirect after delete, got {status}"
    );
}

#[tokio::test]
async fn localized_collection_search_returns_200() {
    let app = setup_localized_app();
    let user_id = create_test_user(&app, "admin@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "admin@test.com");

    {
        let reg = &app.registry;
        let def = reg.get_collection("pages").unwrap().clone();

        let locale_ctx = query::LocaleContext {
            mode: query::LocaleMode::Single("en".to_string()),
            config: make_locale_config(),
        };
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Searchable Page"));
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
        let reg = &app.registry;
        reg.get_collection("articles").unwrap().clone()
    };
    let mut conn = app.pool.get().unwrap();
    let tx = conn.transaction().unwrap();
    let data: DocumentFields = HashMap::from([
        ("title".to_string(), json!("Versioned Article")),
        ("body".to_string(), json!("Content")),
    ])
    .into();
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
        "Create draft should succeed, got {status}"
    );
}

// ── Collections: Search, Filter, Sort, Pagination ─────────────────────────

#[tokio::test]
async fn list_items_with_pagination_renders_docs() {
    let app = setup_app(vec![make_posts_def(), make_users_def()], vec![]);
    let user_id = create_test_user(&app, "page@test.com", "pass123");
    let cookie = make_auth_cookie(&app, &user_id, "page@test.com");

    let def = {
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..25 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Post {}", i)))]).into();
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
    let body = body_string(resp.into_body()).await;
    assert!(body.contains("Page 2 of 3"), "should show page 2 of 3");
    assert!(
        body.contains("Previous"),
        "middle page should have Previous"
    );
    assert!(body.contains("Next"), "middle page should have Next");
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
        let reg = &app.registry;
        reg.get_collection("posts").unwrap().clone()
    };
    for i in 0..5 {
        let mut conn = app.pool.get().unwrap();
        let tx = conn.transaction().unwrap();
        let data: DocumentFields =
            HashMap::from([("title".to_string(), json!(format!("Searchable Item {}", i)))]).into();
        let doc = query::create(&tx, "posts", &def, &data, None).unwrap();
        query::fts::fts_upsert(&tx, "posts", &doc, Some(&def)).unwrap();
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
    // 5 results, per_page=3 → 2 pages with pagination and Next link
    assert!(body.contains("Page 1 of 2"), "should show page info");
    assert!(body.contains("Next"), "should have Next link");
    assert!(
        !body.contains("Previous"),
        "page 1 should not have Previous link"
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
        "Expected 200 (validation error re-render) or redirect, got {status}"
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
        "Create auth collection user should succeed, got {status}"
    );
}

// ── Collections: Create form for auth collection shows password ───────────
