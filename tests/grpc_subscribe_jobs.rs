//! gRPC integration tests for Subscribe streaming and Job RPCs.
//!
//! Uses ContentService directly (no network) via ContentApi trait.

use std::collections::BTreeMap;
use std::sync::Arc;

use prost_types::{Struct, Value, value::Kind};
use tokio_stream::StreamExt;
use tonic::Request;

use crap_cms::api::content;
use crap_cms::api::content::content_api_server::ContentApi;
use crap_cms::api::service::{ContentService, ContentServiceDeps};
use crap_cms::config::*;
use crap_cms::core::Registry;
use crap_cms::core::collection::*;
use crap_cms::core::email::EmailRenderer;
use crap_cms::core::event::EventBus;
use crap_cms::core::field::*;
use crap_cms::core::job::JobDefinitionBuilder;
use crap_cms::db::{migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;
use serde_json::json;

// ── Helpers ───────────────────────────────────────────────────────────────

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
    #[allow(dead_code)]
    pool: crap_cms::db::DbPool,
}

fn setup_service_with_event_bus(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> TestSetup {
    setup_service_inner(collections, globals, Some(EventBus::new(64)), vec![])
}

fn setup_service_with_jobs(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    jobs: Vec<crap_cms::core::job::JobDefinition>,
) -> TestSetup {
    setup_service_inner_with_jobs(collections, globals, vec![], jobs)
}

fn setup_service_inner(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    event_bus: Option<EventBus>,
    locales: Vec<&str>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    if !locales.is_empty() {
        config.locale.locales = locales.iter().map(|s| s.to_string()).collect();
        config.locale.default_locale = locales.first().unwrap_or(&"en").to_string();
        config.locale.fallback = true;
    }

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

    let mut deps = ContentServiceDeps::builder()
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
        .ip_forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )));

    if let Some(eb) = event_bus {
        deps = deps.event_bus(Some(eb));
    }

    let service = ContentService::new(deps.build());

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    }
}

fn setup_service_inner_with_jobs(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    locales: Vec<&str>,
    jobs: Vec<crap_cms::core::job::JobDefinition>,
) -> TestSetup {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();

    if !locales.is_empty() {
        config.locale.locales = locales.iter().map(|s| s.to_string()).collect();
        config.locale.default_locale = locales.first().unwrap_or(&"en").to_string();
        config.locale.fallback = true;
    }

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
        for job in jobs {
            reg.register_job(job);
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

    let deps = ContentServiceDeps::builder()
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
        .ip_forgot_password_limiter(Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )));

    let service = ContentService::new(deps.build());

    TestSetup {
        _tmp: tmp,
        service,
        pool: db_pool,
    }
}

// ── Collection definitions ──────────────────────────────────────────────

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

fn make_tags_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("tags");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Tag".to_string())),
        plural: Some(LocalizedString::Plain("Tags".to_string())),
    };
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text)
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

fn make_simple_global_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.labels = Labels {
        singular: Some(LocalizedString::Plain("Settings".to_string())),
        plural: None,
    };
    def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
    def
}

/// Create a user and log in, returning the JWT token.
async fn create_user_and_login(ts: &TestSetup) -> String {
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

    ts.service
        .login(Request::new(content::LoginRequest {
            collection: "users".to_string(),
            email: "admin@test.com".to_string(),
            password: "admin123".to_string(),
        }))
        .await
        .unwrap()
        .into_inner()
        .token
}

/// Add Bearer token to a Request.
fn add_auth<T>(req: &mut Request<T>, token: &str) {
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", token).parse().unwrap(),
    );
}

// ── Subscribe Tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn subscribe_receives_create_event() {
    let ts = setup_service_with_event_bus(vec![make_posts_def()], vec![]);

    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut stream = resp.into_inner();

    // Create a post — should trigger an event
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Event Post")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("should receive event within timeout")
        .expect("stream should not end")
        .expect("event should be ok");

    assert_eq!(event.target, "collection");
    assert_eq!(event.operation, "create");
    assert_eq!(event.collection, "posts");
    assert!(!event.document_id.is_empty());
}

#[tokio::test]
async fn subscribe_receives_update_event() {
    let ts = setup_service_with_event_bus(vec![make_posts_def()], vec![]);

    // Create first, then subscribe
    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "To Update")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut stream = resp.into_inner();

    // Update the post
    ts.service
        .update(Request::new(content::UpdateRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
            data: Some(make_struct(&[("title", "Updated")])),
            locale: None,
            draft: None,
            unpublish: None,
        }))
        .await
        .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("should receive event within timeout")
        .expect("stream should not end")
        .expect("event should be ok");

    assert_eq!(event.operation, "update");
    assert_eq!(event.document_id, doc.id);
}

#[tokio::test]
async fn subscribe_receives_delete_event() {
    let ts = setup_service_with_event_bus(vec![make_posts_def()], vec![]);

    let doc = ts
        .service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "To Delete")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .document
        .unwrap();

    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut stream = resp.into_inner();

    ts.service
        .delete(Request::new(content::DeleteRequest {
            collection: "posts".to_string(),
            id: doc.id.clone(),
        }))
        .await
        .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("should receive event within timeout")
        .expect("stream should not end")
        .expect("event should be ok");

    assert_eq!(event.operation, "delete");
    assert_eq!(event.document_id, doc.id);
}

#[tokio::test]
async fn subscribe_filters_by_collection() {
    let ts = setup_service_with_event_bus(vec![make_posts_def(), make_tags_def()], vec![]);

    // Subscribe to posts only
    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            collections: vec!["posts".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut stream = resp.into_inner();

    // Create a tag (should NOT trigger event for posts subscriber)
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "tags".to_string(),
            data: Some(make_struct(&[("name", "rust")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    // Create a post (SHOULD trigger event)
    ts.service
        .create(Request::new(content::CreateRequest {
            collection: "posts".to_string(),
            data: Some(make_struct(&[("title", "Only Post")])),
            locale: None,
            draft: None,
        }))
        .await
        .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("should receive event within timeout")
        .expect("stream should not end")
        .expect("event should be ok");

    assert_eq!(
        event.collection, "posts",
        "should only receive events for subscribed collection"
    );
}

#[tokio::test]
async fn subscribe_global_events() {
    let ts = setup_service_with_event_bus(vec![], vec![make_simple_global_def()]);

    let resp = ts
        .service
        .subscribe(Request::new(content::SubscribeRequest {
            globals: vec!["settings".to_string()],
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut stream = resp.into_inner();

    // Update the global
    ts.service
        .update_global(Request::new(content::UpdateGlobalRequest {
            slug: "settings".to_string(),
            data: Some(make_struct(&[("site_name", "My Site")])),
            locale: None,
        }))
        .await
        .unwrap();

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("should receive event within timeout")
        .expect("stream should not end")
        .expect("event should be ok");

    assert_eq!(event.target, "global");
    assert_eq!(event.collection, "settings");
}

// ── Job Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn list_jobs_authenticated() {
    let job = JobDefinitionBuilder::new("cleanup", "hooks.jobs.cleanup")
        .schedule("0 3 * * *")
        .queue("maintenance")
        .retries(3)
        .build();

    let ts = setup_service_with_jobs(vec![make_posts_def(), make_users_def()], vec![], vec![job]);

    let token = create_user_and_login(&ts).await;

    let mut req = Request::new(content::ListJobsRequest {});
    add_auth(&mut req, &token);

    let resp = ts.service.list_jobs(req).await.unwrap().into_inner();
    assert_eq!(resp.jobs.len(), 1);
    assert_eq!(resp.jobs[0].slug, "cleanup");
    assert_eq!(resp.jobs[0].handler, "hooks.jobs.cleanup");
    assert_eq!(resp.jobs[0].queue, "maintenance");
    assert_eq!(resp.jobs[0].retries, 3);
}

#[tokio::test]
async fn trigger_job_authenticated() {
    let job = JobDefinitionBuilder::new("process", "hooks.jobs.process")
        .queue("default")
        .build();

    let ts = setup_service_with_jobs(vec![make_posts_def(), make_users_def()], vec![], vec![job]);

    let token = create_user_and_login(&ts).await;

    let mut req = Request::new(content::TriggerJobRequest {
        slug: "process".to_string(),
        data_json: Some(r#"{"key": "value"}"#.to_string()),
    });
    add_auth(&mut req, &token);

    let resp = ts.service.trigger_job(req).await.unwrap().into_inner();
    assert!(
        !resp.job_id.is_empty(),
        "triggered job should return a job_id"
    );
}

#[tokio::test]
async fn list_job_runs_authenticated() {
    let job = JobDefinitionBuilder::new("sync", "hooks.jobs.sync")
        .queue("default")
        .build();

    let ts = setup_service_with_jobs(vec![make_posts_def(), make_users_def()], vec![], vec![job]);

    let token = create_user_and_login(&ts).await;

    // Trigger a job to create a run
    let mut trigger_req = Request::new(content::TriggerJobRequest {
        slug: "sync".to_string(),
        data_json: None,
    });
    add_auth(&mut trigger_req, &token);
    ts.service.trigger_job(trigger_req).await.unwrap();

    // List runs
    let mut req = Request::new(content::ListJobRunsRequest {
        slug: Some("sync".to_string()),
        status: None,
        limit: None,
        offset: None,
    });
    add_auth(&mut req, &token);

    let resp = ts.service.list_job_runs(req).await.unwrap().into_inner();
    assert_eq!(resp.runs.len(), 1, "should have 1 job run");
    assert_eq!(resp.runs[0].slug, "sync");
    assert_eq!(resp.runs[0].status, "pending");
}
