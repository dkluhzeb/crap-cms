//! Auth-related gRPC integration tests: login, me, password reset,
//! email verification, bearer token, order-by tests.
//!
//! Uses `ContentService` directly (no network) via `ContentApi` trait.

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

use std::collections::HashMap;
use std::sync::Arc;

use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
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
    def.auth = Some(Auth::enabled());
    def
}

/// Build a prost Struct from key-value string pairs.
fn make_struct(pairs: &[(&str, &str)]) -> content::DataMap {
    let mut fields = HashMap::new();
    for (k, v) in pairs {
        fields.insert(
            k.to_string(),
            content::FieldValue {
                kind: Some(content::field_value::Kind::StringValue(v.to_string())),
            },
        );
    }
    content::DataMap { fields }
}

/// Extract a string field from a proto Document's fields struct.
fn get_proto_field(doc: &content::Document, field: &str) -> Option<String> {
    doc.fields.as_ref().and_then(|s| {
        s.fields.get(field).and_then(|v| match &v.kind {
            Some(content::field_value::Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        })
    })
}

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
    pool: crap_cms::db::DbPool,
    /// The same per-IP forgot/reset limiter the service holds, exposed so
    /// rate-limit tests can seed and inspect it.
    ip_forgot_password_limiter: Arc<crap_cms::core::rate_limit::LoginRateLimiter>,
}

fn setup_service(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

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

    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).expect("create email renderer"));

    let ip_forgot_password_limiter =
        Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(20, 900));

    let service = ContentService::new(
        ContentServiceDeps::builder()
            .pool(db_pool.clone())
            .registry(Registry::snapshot(&shared))
            .hook_runner(hook_runner)
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
            .login_limiter(std::sync::Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300),
            ))
            .ip_login_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
                20, 300,
            )))
            .forgot_password_limiter(std::sync::Arc::new(
                crap_cms::core::rate_limit::LoginRateLimiter::new(3, 900),
            ))
            .ip_forgot_password_limiter(Arc::clone(&ip_forgot_password_limiter))
            .cache(std::sync::Arc::new(crap_cms::core::cache::NoneCache))
            .token_provider(std::sync::Arc::new(
                crap_cms::core::auth::JwtTokenProvider::new("test-jwt-secret"),
            ))
            .password_provider(std::sync::Arc::new(
                crap_cms::core::auth::Argon2PasswordProvider,
            ))
            .build(),
    );

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
        ip_forgot_password_limiter,
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
    def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
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
            events: None,
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

/// Regression: gRPC `login` must mint session tokens with the `auth_time`
/// claim set to the original login instant, so `session_absolute_max_age`
/// can cap cumulative session lifetime on refresh. Without it the cap
/// still holds via the `iat` fallback, but loosens by one `token_expiry`
/// interval — which is quiet, unintended drift.
#[tokio::test]
async fn login_token_carries_auth_time() {
    use crap_cms::core::auth::{JwtTokenProvider, TokenProvider};

    let ts = setup_service(vec![make_users_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            events: None,
            collection: "users".to_string(),
            data: Some(make_struct(&[
                ("email", "authtime@example.com"),
                ("name", "Audit"),
                ("password", "secret123"),
            ])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let before = chrono::Utc::now().timestamp() as u64;

    let resp = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "authtime@example.com".to_string(),
            password: "secret123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();

    let after = chrono::Utc::now().timestamp() as u64;

    let provider = JwtTokenProvider::new("test-jwt-secret");
    let claims = provider
        .validate_token(&resp.token)
        .expect("token must validate");

    let auth_time = claims
        .auth_time
        .expect("session token must carry auth_time claim");
    assert!(
        auth_time >= before && auth_time <= after,
        "auth_time {auth_time} outside login window [{before}, {after}]",
    );
}

#[tokio::test]
async fn login_invalid_password() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    ts.service
        .create(Request::new(content::CreateRequest {
            events: None,
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
            events: None,
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

/// Regression: the gRPC `reset_password` IP rate limit must be recorded
/// atomically up front via `check_and_block`, not the old `is_blocked` (read)
/// + later `record_failure` split. Under that split a burst of concurrent
/// resets all observe the same under-limit count before any records, so more
/// than the threshold slip through per window. Seeding the limiter to one below
/// its threshold and firing several genuine (valid-policy, wrong-token)
/// attempts concurrently must leave only one slot — the rest are rejected with
/// `RESOURCE_EXHAUSTED`. The non-atomic version would let all of them pass the
/// gate (zero rejections).
#[tokio::test]
async fn reset_password_ip_limiter_is_atomic_under_concurrency() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Direct service calls carry no remote_addr, so the handler keys the
    // limiter under "unknown" — seed that key. Harness IP limiter is 20/window.
    let key = "unknown";
    for _ in 0..19 {
        let _ = ts.ip_forgot_password_limiter.check_and_block(key);
    }
    assert!(
        !ts.ip_forgot_password_limiter.is_blocked(key),
        "precondition: not yet blocked at 19/20"
    );

    let make_req = || {
        Request::new(content::ResetPasswordRequest {
            collection: "users".to_string(),
            token: "wrong-token".to_string(),
            new_password: "newpass123".to_string(),
        })
    };

    let (res1, res2, res3, res4) = tokio::join!(
        ts.service.reset_password(make_req()),
        ts.service.reset_password(make_req()),
        ts.service.reset_password(make_req()),
        ts.service.reset_password(make_req()),
    );

    let rejected = [res1, res2, res3, res4]
        .iter()
        .filter(|r| matches!(r, Err(s) if s.code() == tonic::Code::ResourceExhausted))
        .count();

    assert!(
        rejected >= 1,
        "atomic check_and_block must reject at least one over-threshold concurrent \
         attempt (got {rejected}); a non-atomic gate lets all four through"
    );
    assert!(
        ts.ip_forgot_password_limiter.is_blocked(key),
        "a genuine wrong-token reset attempt must advance the IP limiter to its threshold"
    );
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

    // A non-error response is the success signal (anti-enumeration).
    ts.service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "posts".to_string(),
            email: "a@b.com".to_string(),
        }))
        .await
        .unwrap();
}

#[tokio::test]
async fn forgot_password_always_returns_success() {
    // ForgotPassword always returns success to avoid leaking user existence
    let ts = setup_service(vec![make_users_def()], vec![]);

    // A non-error response is the success signal (anti-enumeration).
    ts.service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "nonexistent@example.com".to_string(),
        }))
        .await
        .unwrap();
}

#[tokio::test]
async fn forgot_password_not_enabled() {
    // ForgotPassword returns success even when forgot_password is disabled to
    // avoid leaking collection configuration details to potential attackers.
    let mut def = make_users_def();
    if let Some(auth) = def.auth.take() {
        def.auth = Some(auth.map_password_login(|b| b.forgot_password(false)));
    }
    let ts = setup_service(vec![def], vec![]);

    // A non-error response is the success signal (anti-enumeration).
    ts.service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "a@b.com".to_string(),
        }))
        .await
        .unwrap();
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
            events: None,
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
        events: None,
        collection: "posts".to_string(),
        data: Some(make_struct(&[("title", "Authenticated Post")])),
        locale: None,
        draft: None,
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

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
                events: None,
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
                events: None,
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
            events: None,
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

    // Request password reset (always succeeds — non-error response is the signal)
    ts.service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "reset@example.com".to_string(),
        }))
        .await
        .unwrap();

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
    // Per gRPC spec, invalid/missing auth credentials map to UNAUTHENTICATED
    // (code 16), not INVALID_ARGUMENT — see the alpha.8 ServiceError → Status
    // mapping fix. Client SDKs trigger token-refresh on this code.
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn reset_password_expired_token() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create a user
    ts.service
        .create(Request::new(content::CreateRequest {
            events: None,
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
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
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
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
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
            events: None,
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
            events: None,
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
            events: None,
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
            events: None,
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
            trash: None,
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
            events: None,
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

    // Call ForgotPassword with a completely non-existent email. A non-error
    // response is the success signal — it must not leak that the email is
    // unknown by erroring.
    ts.service
        .forgot_password(Request::new(content::ForgotPasswordRequest {
            collection: "users".to_string(),
            email: "does-not-exist@example.com".to_string(),
        }))
        .await
        .unwrap();
}

#[tokio::test]
async fn login_locked_account() {
    let ts = setup_service(vec![make_users_def()], vec![]);

    // Create user
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            events: None,
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
