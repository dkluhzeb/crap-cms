//! gRPC integration tests for `default_deny = true` behavior.
//!
//! Verifies that collections without explicit access functions deny all
//! operations when `access.default_deny = true` (the production default).

use std::sync::Arc;

use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
use crap_cms::config::CrapConfig;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    pool: crap_cms::db::DbPool,
}

/// Set up a gRPC service with `default_deny = true` and NO access functions.
fn setup_default_deny() -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default(); // default_deny = true
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    let mut def = CollectionDefinition::new("posts");
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    // No access functions — access.create, access.read, etc. are all None

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        reg.register_collection(def);
    }

    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(registry.clone())
        .config(&config)
        .build()
        .expect("create hook runner");

    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("email renderer"));

    let deps = ContentServiceDeps::builder()
        .pool(db_pool.clone())
        .registry(Registry::snapshot(&registry))
        .hook_runner(hook_runner)
        .jwt_secret(config.auth.secret.clone())
        .config(config.clone())
        .config_dir(tmp.path().to_path_buf())
        .storage(
            crap_cms::core::upload::create_storage(
                tmp.path(),
                &crap_cms::config::UploadConfig::default(),
            )
            .unwrap(),
        )
        .email_renderer(email_renderer)
        .login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            5, 300,
        )))
        .ip_login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 300,
        )))
        .forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            3, 900,
        )))
        .ip_forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )))
        .cache(Arc::new(crap_cms::core::cache::NoneCache))
        .token_provider(Arc::new(crap_cms::core::auth::JwtTokenProvider::new(
            "test-secret",
        )))
        .password_provider(Arc::new(crap_cms::core::auth::Argon2PasswordProvider));

    let service = ContentService::new(deps.build());

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    }
}

/// Insert a document directly via SQL (bypassing access control) for test setup.
fn insert_test_doc(pool: &crap_cms::db::DbPool, id: &str, title: &str) {
    use crap_cms::db::DbConnection;
    let conn = pool.get().unwrap();
    conn.execute(
        &format!(
            "INSERT INTO \"posts\" (id, title) VALUES ('{}', '{}')",
            id, title
        ),
        &[],
    )
    .unwrap();
}

// ── Tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn default_deny_blocks_find() {
    let ts = setup_default_deny();

    let err = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "Find should be denied with default_deny=true and no access function, got: {}",
        err.message()
    );
}

#[tokio::test]
async fn default_deny_blocks_find_by_id() {
    let ts = setup_default_deny();
    insert_test_doc(&ts.pool, "doc1", "Hello");

    let err = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "posts".to_string(),
            id: "doc1".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "FindByID should be denied, got: {}",
        err.message()
    );
}

#[tokio::test]
async fn default_deny_blocks_create() {
    let ts = setup_default_deny();

    let data = prost_types::Struct {
        fields: [(
            "title".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("Hello".to_string())),
            },
        )]
        .into(),
    };

    let err = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(data),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "Create should be denied, got: {}",
        err.message()
    );
}

#[tokio::test]
async fn default_deny_blocks_delete() {
    let ts = setup_default_deny();
    insert_test_doc(&ts.pool, "doc1", "Hello");

    let err = ts
        .service
        .delete(Request::new(content::DeleteRequest {
            collection: "posts".to_string(),
            id: "doc1".to_string(),
            force_hard_delete: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "Delete should be denied, got: {}",
        err.message()
    );
}

#[tokio::test]
async fn default_deny_blocks_count() {
    let ts = setup_default_deny();

    let err = ts
        .service
        .count(Request::new(content::CountRequest {
            collection: "posts".to_string(),
            ..Default::default()
        }))
        .await
        .unwrap_err();

    assert_eq!(
        err.code(),
        tonic::Code::PermissionDenied,
        "Count should be denied, got: {}",
        err.message()
    );
}
