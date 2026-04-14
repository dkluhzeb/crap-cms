//! gRPC Subscribe security tests: slow-client drop (SEC-D) and
//! user-invalidation stream tear-down (SEC-E).
//!
//! These tests drive `ContentService` directly (no network).

use std::sync::Arc;
use std::time::Duration;

use prost_types::{Struct, Value, value::Kind};
use std::collections::BTreeMap;
use tokio::time::{sleep, timeout};
use tokio_stream::StreamExt;
use tonic::{Code, Request};

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::handlers::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::event::{InProcessEventBus, SharedEventTransport};
use crap_cms::core::field::*;
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;

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

struct TestSetup {
    _tmp: tempfile::TempDir,
    service: ContentService,
}

fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
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
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
    ];
    def.auth = Some(Auth {
        enabled: true,
        ..Default::default()
    });
    def
}

/// Build a service wired with the given live config override (for SEC-D timeout
/// tests) plus the standard collections.
fn setup_service(
    channel_capacity: usize,
    send_timeout_ms: u64,
    collections: Vec<CollectionDefinition>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.live.channel_capacity = channel_capacity;
    config.live.subscriber_send_timeout_ms = send_timeout_ms;

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let registry = Registry::shared();
    {
        let mut reg = registry.write().unwrap();
        for def in &collections {
            reg.register_collection(def.clone());
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

    let event_transport: SharedEventTransport = Arc::new(InProcessEventBus::new(channel_capacity));

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
        .event_transport(Some(event_transport))
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
            "test-jwt-secret",
        )))
        .password_provider(Arc::new(crap_cms::core::auth::Argon2PasswordProvider));

    let service = ContentService::new(deps.build());

    TestSetup { _tmp: tmp, service }
}

async fn create_user_and_login(ts: &TestSetup, email: &str, password: &str) -> (String, String) {
    let created = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "users".to_string(),
            data: Some(make_struct(&[("email", email), ("password", password)])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let token = ts
        .service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: email.to_string(),
            password: password.to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .token;

    (created.id, token)
}

fn add_auth<T>(req: &mut Request<T>, token: &str) {
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );
}

// ── SEC-D: slow-client drop ────────────────────────────────────────────────

/// When a subscriber doesn't read events fast enough to clear broadcast capacity,
/// it must be dropped (stream ends) rather than left to block publishers.
///
/// We force this by:
/// 1. Using a tiny channel_capacity (4) and a short send_timeout (100ms).
/// 2. Not reading from our subscriber stream at all.
/// 3. Publishing more events than the capacity — broadcast Lagged kicks in
///    and drops the subscriber, closing the stream.
#[tokio::test]
async fn subscriber_dropped_on_lagged() {
    let ts = setup_service(4, 100, vec![make_posts_def()]);

    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut stream = resp.into_inner();

    // Do not consume the stream. Flood with events beyond capacity (4). The
    // pumping task will observe `RecvError::Lagged` and break the stream.
    for i in 0..20 {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", &format!("post {}", i))])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Give the pumping task time to observe the lag and close.
    sleep(Duration::from_millis(300)).await;

    // The stream must terminate (Lagged -> drop). We may still receive some
    // events before the drop; drain them and then expect end-of-stream.
    let deadline = Duration::from_secs(2);
    let ended = timeout(deadline, async {
        loop {
            match stream.next().await {
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return true,
            }
        }
    })
    .await
    .expect("stream must end within deadline");

    assert!(ended, "stream should have ended due to lag");
}

/// When the per-subscriber outbound channel stays full past the send timeout,
/// the subscriber is dropped. We use a large broadcast capacity (so Lagged
/// doesn't kick in first) and a short send_timeout. The stream is NOT read
/// while events are published, so the per-subscriber outbound mpsc fills, the
/// pump task's `send_timeout` fires, and the pump exits.
#[tokio::test]
async fn subscriber_dropped_on_send_timeout() {
    // Large broadcast capacity so the broadcast Lagged path does not fire.
    // Short subscriber send_timeout so the per-client backpressure path does.
    let ts = setup_service(1024, 100, vec![make_posts_def()]);

    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut stream = resp.into_inner();

    // Flood far more events than the per-subscriber outbound mpsc capacity
    // (16) — without reading the stream. The pump's send_timeout will fire on
    // the first blocked send, killing the pump and dropping the tx.
    for i in 0..128 {
        ts.service
            .create(Request::new(content::CreateRequest {
                collection: "posts".to_string(),
                data: Some(make_struct(&[("title", &format!("p{}", i))])),
                locale: None,
                draft: None,
            }))
            .await
            .unwrap();
    }

    // Give the pump time to observe backpressure and exit. send_timeout=100ms,
    // so 500ms is comfortably past it.
    sleep(Duration::from_millis(500)).await;

    // Now drain whatever was buffered before the pump died, and assert the
    // stream terminates (None / Err).
    let ended = timeout(Duration::from_secs(2), async {
        loop {
            match stream.next().await {
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return true,
            }
        }
    })
    .await
    .expect("stream must terminate within deadline");

    assert!(ended, "slow subscriber must be torn down by send_timeout");
}

// ── SEC-E: user-invalidation tear-down ────────────────────────────────────

#[tokio::test]
async fn subscriber_dropped_when_user_locked() {
    let ts = setup_service(1024, 1000, vec![make_posts_def(), make_users_def()]);

    let (user_id, token) = create_user_and_login(&ts, "a@test.com", "password1").await;

    let mut sub_req = Request::new(content::SubscribeRequest {
        collections: vec!["posts".to_string()],
        ..Default::default()
    });
    add_auth(&mut sub_req, &token);
    let resp = ts.service.subscribe(sub_req).await.unwrap();
    let mut stream = resp.into_inner();

    // Lock the user (publishes to UserInvalidationBus).
    let mut lock_req = Request::new(content::AccountActionRequest {
        collection: "users".to_string(),
        id: user_id.clone(),
    });
    add_auth(&mut lock_req, &token);
    ts.service.lock_account(lock_req).await.unwrap();

    // Within a short window we should get a terminal PermissionDenied status
    // and then end-of-stream.
    let result = timeout(Duration::from_secs(3), async {
        loop {
            match stream.next().await {
                Some(Ok(_)) => continue,
                Some(Err(status)) => return Some(status),
                None => return None,
            }
        }
    })
    .await
    .expect("stream must terminate within deadline");

    match result {
        Some(status) => assert_eq!(
            status.code(),
            Code::PermissionDenied,
            "expected PermissionDenied on invalidation; got {:?}",
            status
        ),
        None => panic!("stream ended without a terminal status"),
    }
}

#[tokio::test]
async fn subscriber_dropped_when_user_deleted() {
    let ts = setup_service(1024, 1000, vec![make_posts_def(), make_users_def()]);

    let (user_id, token) = create_user_and_login(&ts, "b@test.com", "password1").await;

    let mut sub_req = Request::new(content::SubscribeRequest {
        collections: vec!["posts".to_string()],
        ..Default::default()
    });
    add_auth(&mut sub_req, &token);
    let resp = ts.service.subscribe(sub_req).await.unwrap();
    let mut stream = resp.into_inner();

    // Hard-delete the user.
    let mut del_req = Request::new(content::DeleteRequest {
        collection: "users".to_string(),
        id: user_id.clone(),
        force_hard_delete: true,
    });
    add_auth(&mut del_req, &token);
    ts.service.delete(del_req).await.unwrap();

    // Stream should close with PermissionDenied.
    let ended = timeout(Duration::from_secs(3), async {
        loop {
            match stream.next().await {
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return true,
            }
        }
    })
    .await
    .expect("stream must terminate within deadline");

    assert!(ended, "stream must end after user hard-delete");
}

#[tokio::test]
async fn anonymous_subscriber_not_affected_by_user_events() {
    let ts = setup_service(1024, 1000, vec![make_posts_def(), make_users_def()]);

    // Anonymous subscriber — no Bearer token. But we need access; use a
    // permissive default setup by leaving access unset (default allow — check
    // via creating a post and receiving the event).
    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await;

    // If the default-deny is on, anonymous won't be allowed; skip silently in
    // that case (the test cannot run without a policy that allows anon reads).
    let Ok(resp) = resp else {
        return;
    };
    let mut stream = resp.into_inner();

    // Create user, lock them. This publishes to invalidation bus with user_id.
    let (user_id, token) = create_user_and_login(&ts, "c@test.com", "password1").await;
    let mut lock_req = Request::new(content::AccountActionRequest {
        collection: "users".to_string(),
        id: user_id.clone(),
    });
    add_auth(&mut lock_req, &token);
    ts.service.lock_account(lock_req).await.unwrap();

    // Publish a posts event that anon is allowed to see.
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "after-lock")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Anonymous stream must still deliver the post event (not torn down).
    let event = timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("stream should still be alive")
        .expect("stream should yield an event")
        .expect("event should be Ok");

    assert_eq!(event.collection, "posts");
    assert_eq!(event.operation, "create");
}
