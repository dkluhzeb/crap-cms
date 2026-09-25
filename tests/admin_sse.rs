//! Integration tests for the admin `/admin/events` SSE endpoint.
//!
//! Every other admin integration test builds its app with
//! `event_transport: None` and `max_sse_connections: 0`, so the streaming half
//! of the live-update surface — the per-subscriber pump, the per-collection
//! access gate, the connection cap — never ran end to end. These tests drive the
//! real router: open the stream, write a document through the normal admin form
//! POST, and read the frames the endpoint actually puts on the wire.
//!
//! The payload shaping itself (view gating, field stripping, editor-identity
//! suppression) is unit-tested in `src/admin/handlers/events/sse_payload.rs`;
//! what is covered here is the transport: does a write reach a connected
//! subscriber at all, does a collection the subscriber cannot read stay off the
//! stream, and does the slot counter admit and release connections.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use handlebars::Handlebars;
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crap_cms::{
    admin::{
        AdminState, Translations, custom_pages::CustomPageRegistry, server::build_router, templates,
    },
    config::{CrapConfig, EmailConfig, UploadConfig},
    core::{
        CollectionDefinition, FieldDefinition, FieldType, HookRef, LiveMode, LiveSlots, Registry,
        SharedEventTransport, SharedTokenProvider,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        email::create_email_provider,
        event::InProcessEventBus,
        rate_limit::LoginRateLimiter,
        upload::create_storage,
    },
    db::{DbPool, migrate, pool},
    hooks::HookRunner,
    service::{AppInfra, StandaloneInfra},
};

/// Fixed CSRF token — the double-submit middleware only compares the cookie
/// against the header, so any stable value works.
const TEST_CSRF: &str = "test-csrf-token-12345";

/// How long a test waits for an event to travel transport → pump → wire.
const STREAM_WAIT: Duration = Duration::from_secs(5);

// ── Fixtures ──────────────────────────────────────────────────────────────

/// A readable collection streaming full document data, so the assertions can
/// check the payload and not just the envelope.
fn make_posts_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("posts");
    def.timestamps = true;
    def.live_mode = LiveMode::Full;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .required(true)
            .build(),
    ];
    def
}

/// A collection whose `read` access always denies. Creating into it is still
/// allowed (`create` access is separate), so a write produces an event that the
/// subscriber must never see.
fn make_vault_def() -> CollectionDefinition {
    let mut def = make_posts_def();
    def.slug = "vault".into();
    def.access.read = Some(HookRef::new("access.deny_all"));
    def
}

/// Write the Lua `read` rule `make_vault_def` points at into the config dir the
/// hook runner loads from.
fn write_deny_all_hook(dir: &Path) {
    let access_dir = dir.join("access");
    std::fs::create_dir_all(&access_dir).expect("create access dir");

    std::fs::write(
        access_dir.join("deny_all.lua"),
        "return function(ctx) return false end\n",
    )
    .expect("write deny_all hook");
}

// ── App construction ──────────────────────────────────────────────────────

struct TestApp {
    _tmp: TempDir,
    router: Router,
}

/// Everything [`build_state`] needs. All fields are required and it is built in
/// exactly one place, so a plain struct literal stands in for a builder.
struct StateParts {
    infra: Arc<AppInfra>,
    config: CrapConfig,
    config_dir: PathBuf,
    handlebars: Arc<Handlebars<'static>>,
    translations: Arc<Translations>,
    max_sse_connections: usize,
}

fn build_state(parts: StateParts) -> AdminState {
    AdminState {
        mcp_sessions: Arc::default(),
        infra: parts.infra,
        config: parts.config,
        config_dir: parts.config_dir,
        handlebars: parts.handlebars,
        jwt_secret: "test-jwt-secret".into(),
        email_provider: create_email_provider(&EmailConfig::default()).expect("email provider"),
        login_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_login_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: Arc::new(LoginRateLimiter::new(3, 900)),
        ip_forgot_password_limiter: Arc::new(LoginRateLimiter::new(20, 900)),
        mfa_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        // No auth collection is registered and `require_auth` stays false, so
        // the admin routes are reachable anonymously and the subscriber is the
        // anonymous user.
        has_auth: false,
        translations: parts.translations,
        sse_slots: LiveSlots::new(parts.max_sse_connections, 0),
        shutdown: CancellationToken::new(),
        password_provider: Arc::new(Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: CustomPageRegistry::default(),
    }
}

/// A file-backed pool in `dir` with `collections` registered and migrated.
fn migrated_pool(
    dir: &Path,
    config: &CrapConfig,
    collections: Vec<CollectionDefinition>,
) -> (DbPool, Arc<Registry>) {
    let db_pool = pool::create_pool(dir, config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().expect("registry lock");
        for def in collections {
            reg.register_collection(def);
        }
    }

    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    (db_pool, registry)
}

/// Build the full admin router over `collections`. `transport` is what the SSE
/// handler subscribes to (`None` = live updates off) and `max_sse_connections`
/// is the slot cap (0 = unlimited). The [`TempDir`] doubles as the config dir,
/// so a caller may pre-populate it with Lua hooks before this runs.
fn build_app(
    collections: Vec<CollectionDefinition>,
    tmp: TempDir,
    transport: Option<SharedEventTransport>,
    max_sse_connections: usize,
) -> TestApp {
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = "test-jwt-secret".into();
    config.admin.require_auth = false;

    let (db_pool, registry) = migrated_pool(tmp.path(), &config, collections);

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("create hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars =
        templates::create_handlebars(tmp.path(), false, Arc::clone(&translations), None)
            .expect("create handlebars");
    let storage = create_storage(tmp.path(), &UploadConfig::default()).expect("create storage");
    let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new("test-jwt-secret"));

    let infra = AppInfra::standalone(StandaloneInfra {
        pool: db_pool,
        registry,
        hook_runner,
        storage,
        token_provider: Some(token_provider),
        event_transport: transport,
        invalidation_transport: None,
        config: &config,
        config_dir: tmp.path(),
    })
    .expect("build test infra");

    let state = build_state(StateParts {
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        handlebars,
        translations,
        max_sse_connections,
    });

    TestApp {
        router: build_router(state),
        _tmp: tmp,
    }
}

/// The common case: live events on, no connection cap.
fn setup_live_app(collections: Vec<CollectionDefinition>) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");
    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));

    build_app(collections, tmp, Some(transport), 0)
}

// ── Requests ──────────────────────────────────────────────────────────────

fn sse_request() -> Request<Body> {
    Request::get("/admin/events")
        .body(Body::empty())
        .expect("build SSE request")
}

/// An admin form create. `title` must already be form-encoded.
fn create_request(slug: &str, title: &str) -> Request<Body> {
    Request::post(format!("/admin/collections/{slug}"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("crap_csrf={TEST_CSRF}"))
        .header("X-CSRF-Token", TEST_CSRF)
        .body(Body::from(format!("title={title}")))
        .expect("build create request")
}

/// Run a create through the router and return the new document's id, taken from
/// the `X-Created-Id` header the success response carries.
async fn create_document(app: &TestApp, slug: &str, title: &str) -> String {
    let resp = app
        .router
        .clone()
        .oneshot(create_request(slug, title))
        .await
        .expect("create request");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "create in '{slug}' must succeed for the event to exist at all"
    );

    resp.headers()
        .get("X-Created-Id")
        .and_then(|v| v.to_str().ok())
        .expect("create response must carry X-Created-Id")
        .to_string()
}

// ── Stream reading ────────────────────────────────────────────────────────

/// Read `body` until one complete SSE event (terminated by a blank line) has
/// arrived, returning it without the trailing blank line. `None` when the
/// stream ends first.
async fn read_sse_event(body: &mut Body) -> Option<String> {
    let mut buf = String::new();

    while let Some(frame) = body.frame().await {
        let Ok(chunk) = frame.ok()?.into_data() else {
            continue;
        };

        buf.push_str(&String::from_utf8_lossy(&chunk));

        if let Some(end) = buf.find("\n\n") {
            buf.truncate(end);
            return Some(buf);
        }
    }

    None
}

/// Wait up to [`STREAM_WAIT`] for the next SSE event on `body`.
async fn next_sse_event(body: &mut Body) -> String {
    timeout(STREAM_WAIT, read_sse_event(body))
        .await
        .expect("timed out waiting for an SSE event")
        .expect("stream ended before an event arrived")
}

/// The JSON object carried by a raw SSE event's `data:` field.
fn sse_data(raw: &str) -> Value {
    let line = raw
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .unwrap_or_else(|| panic!("SSE event carries no data field: {raw:?}"));

    serde_json::from_str(line).expect("SSE data must be JSON")
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// The end-to-end live path: a subscriber connected to `/admin/events` receives
/// the mutation event for a document written through the normal admin form POST,
/// with the envelope and (in `LiveMode::Full`) the document data intact.
#[tokio::test]
async fn sse_stream_delivers_event_for_document_write() {
    let app = setup_live_app(vec![make_posts_def()]);

    // The handler subscribes to the transport before it returns, so anything
    // published after this point is guaranteed to reach the pump.
    let resp = app
        .router
        .clone()
        .oneshot(sse_request())
        .await
        .expect("SSE request");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
    );

    let mut body = resp.into_body();

    let doc_id = create_document(&app, "posts", "hello-sse").await;

    let raw = next_sse_event(&mut body).await;
    assert!(
        raw.contains("event: mutation"),
        "the stream must name the event type: {raw:?}"
    );

    let data = sse_data(&raw);
    assert_eq!(data["target"], "collection");
    assert_eq!(data["operation"], "create");
    assert_eq!(data["collection"], "posts");
    assert_eq!(data["document_id"], doc_id.as_str());
    assert_eq!(
        data["data"]["title"], "hello-sse",
        "LiveMode::Full must carry the document data: {data}"
    );
}

/// Per-collection gating on the live path: a subscriber that cannot read a
/// collection never sees its events. The denied write happens FIRST, so an
/// ungated stream would deliver it before the readable one — the readable event
/// arriving first is what proves the gate ran.
#[tokio::test]
async fn sse_stream_withholds_events_for_read_denied_collection() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_deny_all_hook(tmp.path());

    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
    let app = build_app(
        vec![make_posts_def(), make_vault_def()],
        tmp,
        Some(transport),
        0,
    );

    let resp = app
        .router
        .clone()
        .oneshot(sse_request())
        .await
        .expect("SSE request");
    assert_eq!(resp.status(), StatusCode::OK);

    let mut body = resp.into_body();

    create_document(&app, "vault", "secret-first").await;
    let visible_id = create_document(&app, "posts", "visible-second").await;

    let data = sse_data(&next_sse_event(&mut body).await);
    assert_eq!(
        data["collection"], "posts",
        "the read-denied collection's event must not reach the stream: {data}"
    );
    assert_eq!(data["document_id"], visible_id.as_str());
}

/// The connection cap: once `max_sse_connections` slots are taken the endpoint
/// refuses further streams with 503, and a disconnect frees the slot again.
#[tokio::test]
async fn sse_connection_cap_rejects_when_full_and_frees_slot_on_disconnect() {
    let transport: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
    let app = build_app(
        vec![make_posts_def()],
        tempfile::tempdir().expect("tempdir"),
        Some(transport),
        1,
    );

    let held = app
        .router
        .clone()
        .oneshot(sse_request())
        .await
        .expect("first SSE request");
    assert_eq!(held.status(), StatusCode::OK, "the only slot is available");

    let rejected = app
        .router
        .clone()
        .oneshot(sse_request())
        .await
        .expect("second SSE request");
    assert_eq!(
        rejected.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a second stream must be refused while the single slot is held"
    );

    // Dropping the response drops the stream, and with it the RAII slot guard.
    drop(held);

    let readmitted = app
        .router
        .clone()
        .oneshot(sse_request())
        .await
        .expect("third SSE request");
    assert_eq!(
        readmitted.status(),
        StatusCode::OK,
        "the slot must be released when a subscriber disconnects"
    );
}

/// With live updates off (`event_transport: None` — what every other admin test
/// builds) the endpoint still answers, but the stream is empty and ends at once
/// rather than hanging a client on a connection that can never deliver.
#[tokio::test]
async fn sse_stream_ends_immediately_without_event_transport() {
    let app = build_app(
        vec![make_posts_def()],
        tempfile::tempdir().expect("tempdir"),
        None,
        0,
    );

    let resp = app
        .router
        .clone()
        .oneshot(sse_request())
        .await
        .expect("SSE request");
    assert_eq!(resp.status(), StatusCode::OK);

    let mut body = resp.into_body();

    let ended = timeout(STREAM_WAIT, read_sse_event(&mut body))
        .await
        .expect("an event-less stream must terminate, not hang");
    assert!(
        ended.is_none(),
        "no transport means no events on the wire, got {ended:?}"
    );
}
