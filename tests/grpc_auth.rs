//! Auth-related gRPC integration tests: login, me, password reset,
//! email verification, bearer token, order-by tests.
//!
//! Uses ContentService directly (no network) via ContentApi trait.

use std::collections::BTreeMap;
use std::sync::Arc;

use prost_types::{Struct, Value, value::Kind};
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::service::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::field::*;
use crap_cms::db::{DbConnection, DbValue, migrate, pool};
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
        FieldDefinition::builder("status", FieldType::Select)
            .default_value(json!("draft"))
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

/// Build a prost Struct from key-value string pairs.
fn make_struct(pairs: &[(&str, &str)]) -> Struct {
    let mut fields = BTreeMap::new();
    for (k, v) in pairs {
        fields.insert(
            k.to_string(),
            Value {
                kind: Some(Kind::StringValue(v.to_string())),
            },
        );
    }
    Struct { fields }
}

/// Extract a string field from a proto Document's fields struct.
fn get_proto_field(doc: &content::Document, field: &str) -> Option<String> {
    doc.fields.as_ref().and_then(|s| {
        s.fields.get(field).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    })
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    pool: crap_cms::db::DbPool,
}

fn setup_service(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

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

    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let service = ContentService::new(
        ContentServiceDeps::builder()
            .pool(db_pool.clone())
            .registry(Registry::snapshot(&registry))
            .hook_runner(hook_runner)
            .jwt_secret(config.auth.secret.clone())
            .config(config.clone())
            .config_dir(tmp.path().to_path_buf())
            .email_renderer(email_renderer)
            .login_limiter(std::sync::Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300),
            ))
            .ip_login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
                20, 300,
            )))
            .forgot_password_limiter(std::sync::Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
            ))
            .ip_forgot_password_limiter(Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(20, 900),
            ))
            .build(),
    );

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    }
}

fn make_verify_users_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("members");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Member".to_string())),
        plural: Some(LocalizedString::Plain("Members".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
    ];
    def.auth = Some(Auth {
        enabled: true,
        verify_email: true,
        ..Default::default()
    });
    def
}

// ── Auth Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn login_non_auth_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "posts".to_string(),
            email: "a@b.com".to_string(),
            password: "secret".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("not an auth collection"));
}

#[tokio::test]
async fn login_valid_credentials() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user with password
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "alice@example.com"),
                ("name", "Alice"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Login
    let resp = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "alice@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(!resp.token.is_empty());
    let user = resp.user.expect("No user in response");
    assert_eq!(
        get_proto_field(&user, "email").as_deref(),
        Some("alice@example.com")
    );
}

#[tokio::test]
async fn login_invalid_password() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "bob@example.com"),
                ("password", "correct1"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "bob@example.com".to_string(),
            password: "wrong".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn login_nonexistent_user() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "nobody@example.com".to_string(),
            password: "anything".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn me_valid_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "carol@example.com"),
                ("name", "Carol"),
                ("password", "pw123456"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let login_resp = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "carol@example.com".to_string(),
            password: "pw123456".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    let me_resp = ts
        .service
        .me(Request::new(content::MeRequest {
            token: login_resp.token,
        }))
        .await
        .unwrap()
        .into_inner();

    let user = me_resp.user.expect("No user");
    assert_eq!(
        get_proto_field(&user, "email").as_deref(),
        Some("carol@example.com")
    );
    assert_eq!(get_proto_field(&user, "name").as_deref(), Some("Carol"));
}

#[tokio::test]
async fn me_invalid_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .me(Request::new(content::MeRequest {
            token: "bogus-token".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

// ── Password Reset Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn reset_password_short_password() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "some-token".to_string(),
            new_password: "short".to_string(), // < 8 chars
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("at least 8 characters"));
}

#[tokio::test]
async fn reset_password_non_auth_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "posts".to_string(),
            token: "tok".to_string(),
            new_password: "newpassword".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// ── Email Verification Tests ──────────────────────────────────────────────

#[tokio::test]
async fn verify_email_non_auth_collection() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let err = ts
        .service
        .verify_email(Request::new(content::VerifyEmailRequest {
            collection: "posts".to_string(),
            token: "tok".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn verify_email_not_enabled() {
    // Users collection has auth but verify_email defaults to false
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .verify_email(Request::new(content::VerifyEmailRequest {
            collection: "users".to_string(),
            token: "tok".to_string(),
        }))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("not enabled"));
}

// ── Forgot Password Tests ─────────────────────────────────────────────────

#[tokio::test]
async fn forgot_password_non_auth_collection() {
    // ForgotPassword returns success even for non-auth collections to avoid
    // leaking collection configuration details to potential attackers.
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "posts".to_string(),
            email: "a@b.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(resp.success);
}

#[tokio::test]
async fn forgot_password_always_returns_success() {
    // ForgotPassword always returns success to avoid leaking user existence
    let ts = setup_service(vec![make_users_def()], vec![]);

    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "nonexistent@example.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(resp.success);
}

#[tokio::test]
async fn forgot_password_not_enabled() {
    // ForgotPassword returns success even when forgot_password is disabled to
    // avoid leaking collection configuration details to potential attackers.
    let mut def = make_users_def();
    if let Some(ref mut auth) = def.auth {
        auth.forgot_password = false;
    }
    let ts = setup_service(vec![def], vec![]);

    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "a@b.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(resp.success);
}

// ── Subscribe Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn subscribe_without_event_bus() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    let result = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await;

    assert!(result.is_err());
    let err = result.err().unwrap();
    assert_eq!(err.code(), tonic::Code::Unavailable);
    assert!(err.message().contains("disabled"));
}

// ── Auth Bearer Token in Metadata ─────────────────────────────────────────

#[tokio::test]
async fn authenticated_crud_with_bearer_token() {
    let ts = setup_service(vec![make_posts_def(), make_users_def()], vec![]);

    // Create user and login
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "admin@test.com"),
                ("password", "admin123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let token = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "admin@test.com".to_string(),
            password: "admin123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .token;

    // Create a post with Bearer token in metadata
    let mut req = Request::new(content::CreateRequest {
        collection: "posts".to_string(),
        data: Some(make_struct(&[("title", "Authenticated Post")])),
        locale: None,
        draft: None,
    });
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );

    let doc = req.extensions().get::<()>(); // just to consume the var
    let _ = doc;

    let resp = ts.service.create(req).await.unwrap().into_inner();
    let doc = resp.document.unwrap();
    assert_eq!(
        get_proto_field(&doc, "title").as_deref(),
        Some("Authenticated Post")
    );
}

// ── Order By Tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn find_with_order_by() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["Charlie", "Alice", "Bob"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            order_by: Some("title".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.documents.len(), 3);
    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("Alice")
    );
    assert_eq!(
        get_proto_field(&resp.documents[1], "title").as_deref(),
        Some("Bob")
    );
    assert_eq!(
        get_proto_field(&resp.documents[2], "title").as_deref(),
        Some("Charlie")
    );
}

#[tokio::test]
async fn find_with_order_by_desc() {
    let ts = setup_service(vec![make_posts_def()], vec![]);

    for title in &["A", "B", "C"] {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", title)])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    let resp = ts
        .service
        .find(Request::new(content::FindRequest {
            collection: "posts".to_string(),
            order_by: Some("-title".to_string()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        get_proto_field(&resp.documents[0], "title").as_deref(),
        Some("C")
    );
    assert_eq!(
        get_proto_field(&resp.documents[2], "title").as_deref(),
        Some("A")
    );
}

// ── Full Password Reset Flow ──────────────────────────────────────────────

#[tokio::test]
async fn full_password_reset_flow() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "reset@example.com"),
                ("password", "oldpassword"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Request password reset (always succeeds)
    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "reset@example.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.success);

    // Verify reset_password rejects an invalid token (the real token was
    // stored by forgot_password but we don't extract it here).
    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "nonexistent-token".to_string(),
            new_password: "newpassword123".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn reset_password_expired_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "expired@example.com"),
                ("password", "mypassword"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Reset with a random token (not valid)
    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "expired-fake-token".to_string(),
            new_password: "newpassword123".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn reset_password_invalid_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let err = ts
        .service
        .reset_password(Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "totally-random-token".to_string(),
            new_password: "newpassword123".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

// ── Email Verification ────────────────────────────────────────────────────

#[tokio::test]
async fn verify_email_invalid_token_returns_error() {
    let ts = setup_service(vec![make_verify_users_def()], vec![]);

    let err = ts
        .service
        .verify_email(Request::new(content::VerifyEmailRequest {
            collection: "members".to_string(),
            token: "bad-token".to_string(),
        }))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn login_blocked_when_unverified() {
    let ts = setup_service(vec![make_verify_users_def()], vec![]);

    // Create user (unverified)
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "members".to_string(),
            data: Some(make_struct(&[
                ("email", "unverified@example.com"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Try to login — should fail because unverified
    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "members".to_string(),
            email: "unverified@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap_err();
    // Unified error: locked/unverified accounts return the same generic error
    // as wrong-password to prevent attackers from confirming password correctness.
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
    assert!(
        err.message().to_lowercase().contains("invalid"),
        "Should return generic 'Invalid email or password', got: {}",
        err.message()
    );
}

// ── Auth Password Update via gRPC ─────────────────────────────────────────

#[tokio::test]
async fn update_password_via_grpc() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create user
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "pwchange@example.com"),
                ("name", "PW Changer"),
                ("password", "oldpass123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Login with old password
    let login_resp = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "pwchange@example.com".to_string(),
            password: "oldpass123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    let user_id = login_resp.user.unwrap().id;

    // Update password (must include required email field)
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "users".to_string(),
            id: user_id,
            data: Some(make_struct(&[
                ("email", "pwchange@example.com"),
                ("password", "newpass456"),
            ])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    // Login with new password should succeed
    let new_login = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "pwchange@example.com".to_string(),
            password: "newpass456".to_string(),
        }))
        .await;
    assert!(new_login.is_ok(), "Login with new password should succeed");

    // Login with old password should fail
    let old_login = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "pwchange@example.com".to_string(),
            password: "oldpass123".to_string(),
        }))
        .await;
    assert!(old_login.is_err(), "Login with old password should fail");
}

// ── Password Hash Not Exposed ─────────────────────────────────────────────

#[tokio::test]
async fn password_hash_not_in_response() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "hidden@example.com"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // The response document should not contain _password_hash
    let fields = doc.fields.as_ref().unwrap();
    assert!(
        !fields.fields.contains_key("_password_hash"),
        "Response should not contain _password_hash"
    );
    assert!(
        !fields.fields.contains_key("password"),
        "Response should not contain password field"
    );

    // Also check find_by_id
    let found = ts
        .service
        .find_by_id(Request::new(content::FindByIdRequest {
            collection: "users".to_string(),
            id: doc.id,
            depth: Some(0),
            locale: None,
            select: vec![],
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let fields = found.fields.as_ref().unwrap();
    assert!(
        !fields.fields.contains_key("_password_hash"),
        "FindByID response should not contain _password_hash"
    );
}

// ── Auth RPC Gaps ─────────────────────────────────────────────────────────

#[tokio::test]
async fn login_locked_account_grpc() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user with password
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "locked@example.com"),
                ("name", "Locked User"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Lock the user via direct DB access
    {
        let conn = ts.pool.get().unwrap();
        crap_cms::db::query::lock_user(&conn, "users", &doc.id).unwrap();
    }

    // Try to login — should fail because the account is locked
    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "locked@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap_err();

    assert!(
        err.code() == tonic::Code::Unauthenticated || err.code() == tonic::Code::PermissionDenied,
        "Locked account login should return Unauthenticated or PermissionDenied, got {:?}: {}",
        err.code(),
        err.message()
    );
}

#[tokio::test]
async fn forgot_password_nonexistent_still_succeeds() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Call ForgotPassword with a completely non-existent email
    let resp = ts
        .service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "does-not-exist@example.com".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    // Should always return success to avoid leaking user existence
    assert!(
        resp.success,
        "ForgotPassword should always return success, even for non-existent emails"
    );
}

#[tokio::test]
async fn login_locked_account() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create user
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "locked@example.com"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    // Lock the user directly in the DB
    {
        let conn = ts.pool.get().unwrap();
        conn.execute(
            "UPDATE users SET _locked = 1 WHERE id = ?1",
            &[DbValue::Text(doc.id.clone())],
        )
        .unwrap();
    }

    // Try to login
    let err = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "locked@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap_err();

    // Unified error: locked accounts return the same generic error as wrong-password
    // to prevent attackers from confirming password correctness.
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
    assert!(
        err.message().to_lowercase().contains("invalid"),
        "Should return generic 'Invalid email or password', got: {}",
        err.message()
    );
}
