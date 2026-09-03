//! Integration tests for signed upload URLs — the `exp`/`sig` capability
//! query parameters on the `/uploads/{collection}/{filename}` serve route.

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

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crap_cms::admin::AdminState;
use crap_cms::admin::server::build_router;
use crap_cms::admin::templates;
use crap_cms::admin::translations::Translations;
use crap_cms::config::CrapConfig;
use serde_json::json;

use crap_cms::core::DocumentFields;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::upload::{CollectionUpload, signed_upload_url};
use crap_cms::core::{HookRef, Registry};
use crap_cms::db::{migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;

const SECRET: &str = "test-jwt-secret";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Public collection: no access hook, no drafts, no soft-delete — the serve
/// route's provably-public fast path.
fn public_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pub_files");
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    def
}

/// Private upload collection: `access.read` hook denies anonymous viewers,
/// and an owning document (created in `setup`) references the served file —
/// so the unsigned 404 exercises the real hook gate, not the orphan path.
fn private_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("priv_files");
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    def.access.read = Some(HookRef::new("hooks.gate.auth_only"));

    let mut upload = CollectionUpload::new();
    upload.enabled = true;
    def.upload = Some(upload);

    def
}

/// Like `priv_files` but no owning document is ever created — the served
/// file is an orphan, hidden by the fail-closed doc-existence check.
fn orphan_def() -> CollectionDefinition {
    let mut def = private_def();
    def.slug = "orphan_files".into();
    def
}

struct TestApp {
    _tmp: tempfile::TempDir,
    router: axum::Router,
}

fn setup() -> TestApp {
    setup_app(false)
}

fn setup_app(default_deny: bool) -> TestApp {
    let tmp = tempfile::tempdir().expect("tempdir");

    std::fs::create_dir_all(tmp.path().join("hooks")).unwrap();
    std::fs::write(
        tmp.path().join("hooks/gate.lua"),
        "local M = {}\nfunction M.auth_only(ctx)\n    return ctx.user ~= nil\nend\nreturn M\n",
    )
    .unwrap();

    // The files the tests serve.
    for (slug, file) in [
        ("priv_files", "a_secret.txt"),
        ("orphan_files", "lost.txt"),
        ("pub_files", "open.txt"),
    ] {
        let dir = tmp.path().join("uploads").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), b"file-bytes").unwrap();
    }

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = SECRET.into();
    config.admin.require_auth = false;
    config.access.default_deny = default_deny;

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(public_def());
        reg.register_collection(private_def());
        reg.register_collection(orphan_def());
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    // The owning document for the private file — so the unsigned request is
    // denied by the ACCESS HOOK (anonymous viewer), not by the orphan check.
    {
        let def = registry.get_collection("priv_files").unwrap().clone();
        let conn = db_pool.get().unwrap();
        let fields: DocumentFields = [
            ("title".to_string(), json!("secret doc")),
            ("url".to_string(), json!(PRIV_PATH)),
        ]
        .into_iter()
        .collect();
        query::create(&conn, "priv_files", &def, &fields, None).expect("create owning doc");
    }

    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    let translations = Arc::new(Translations::load(tmp.path()));
    let handlebars = templates::create_handlebars(tmp.path(), false, translations.clone(), None)
        .expect("handlebars");

    let storage = crap_cms::core::upload::create_storage(
        tmp.path(),
        &crap_cms::config::UploadConfig::default(),
    )
    .unwrap();
    let token_provider: crap_cms::core::SharedTokenProvider =
        Arc::new(crap_cms::core::auth::JwtTokenProvider::new(SECRET));
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
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        handlebars,
        jwt_secret: SECRET.into(),
        email_provider: crap_cms::core::email::create_email_provider(
            &crap_cms::config::EmailConfig::default(),
        )
        .unwrap(),
        login_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        ip_login_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            3, 900,
        )),
        ip_forgot_password_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(
            20, 900,
        )),
        mfa_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: Arc::new(crap_cms::core::rate_limit::LoginRateLimiter::new(20, 300)),
        has_auth: false,
        translations,
        sse_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: tokio_util::sync::CancellationToken::new(),
        password_provider: Arc::new(crap_cms::core::auth::Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: crap_cms::admin::custom_pages::CustomPageRegistry::default(),
    };

    let router = build_router(state);

    TestApp { _tmp: tmp, router }
}

async fn get(app: &TestApp, uri: &str) -> (StatusCode, String) {
    let resp = app
        .router
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    let cache = resp
        .headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    (resp.status(), cache)
}

const PRIV_PATH: &str = "/uploads/priv_files/a_secret.txt";

/// Baseline: the file HAS an owning document, but the collection's read
/// hook denies anonymous viewers — the hook gate hides it.
#[tokio::test]
async fn private_file_hidden_without_auth() {
    let app = setup();
    let (status, _) = get(&app, PRIV_PATH).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A file with no owning document is hidden by the fail-closed orphan
/// check — and a signed capability still serves it (mint-time
/// authorization is the whole contract; documented semantics).
#[tokio::test]
async fn orphan_file_gate_and_capability() {
    let app = setup();
    let path = "/uploads/orphan_files/lost.txt";

    let (status, _) = get(&app, path).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let url = signed_upload_url(SECRET, path, 300, now()).unwrap();
    let (status, _) = get(&app, &url).await;
    assert_eq!(status, StatusCode::OK);
}

/// Under `default_deny = true` (the production default) a hook-less
/// collection denies reads — but a signed capability still serves.
#[tokio::test]
async fn default_deny_blocks_anon_but_signed_serves() {
    let app = setup_app(true);
    let path = "/uploads/pub_files/open.txt";

    let (status, _) = get(&app, path).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let url = signed_upload_url(SECRET, path, 300, now()).unwrap();
    let (status, cache) = get(&app, &url).await;
    assert_eq!(status, StatusCode::OK);
    assert!(cache.starts_with("private, max-age="), "{cache}");
}

/// A valid signature serves the private file with no cookie/Bearer, cached
/// privately and bounded by the signature's remaining validity.
#[tokio::test]
async fn signed_url_serves_private_file() {
    let app = setup();
    let url = signed_upload_url(SECRET, PRIV_PATH, 300, now()).unwrap();

    let (status, cache) = get(&app, &url).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        cache.starts_with("private, max-age="),
        "signed serve must be privately cacheable, got {cache}"
    );
}

/// An expired signature grants nothing — falls through to the normal gate.
#[tokio::test]
async fn expired_signature_is_rejected() {
    let app = setup();
    // Signed 400s ago with a 100s TTL → expired 300s ago.
    let url = signed_upload_url(SECRET, PRIV_PATH, 100, now() - 400).unwrap();

    let (status, _) = get(&app, &url).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A tampered signature grants nothing.
#[tokio::test]
async fn tampered_signature_is_rejected() {
    let app = setup();
    let url = signed_upload_url(SECRET, PRIV_PATH, 300, now()).unwrap();
    let tampered = format!("{}zzzz", &url[..url.len() - 4]);

    let (status, _) = get(&app, &tampered).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A signature minted for one file cannot fetch another.
#[tokio::test]
async fn signature_is_bound_to_the_path() {
    let app = setup();
    let other = signed_upload_url(SECRET, "/uploads/priv_files/other.txt", 300, now()).unwrap();
    let query = other.split_once('?').unwrap().1;

    let (status, _) = get(&app, &format!("{PRIV_PATH}?{query}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Unknown query parameters don't break verification.
#[tokio::test]
async fn extra_query_params_are_ignored() {
    let app = setup();
    let url = signed_upload_url(SECRET, PRIV_PATH, 300, now()).unwrap();

    let (status, _) = get(&app, &format!("{url}&foo=1")).await;
    assert_eq!(status, StatusCode::OK);
}

/// A garbage signature on a public file falls through to the public fast
/// path — an invalid signature never *removes* access.
#[tokio::test]
async fn garbage_signature_on_public_file_falls_through() {
    let app = setup();
    let (status, cache) = get(&app, "/uploads/pub_files/open.txt?exp=1&sig=zz").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        cache.contains("public"),
        "public fast path applies: {cache}"
    );
}
