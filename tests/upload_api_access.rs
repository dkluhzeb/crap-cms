//! Integration tests for the access gate of `POST /api/upload/{slug}`: the
//! collection's rule is judged on the request together with the file it
//! carries, through the full router.

#![allow(clippy::missing_panics_doc)]

use std::{
    fs,
    path::Path,
    sync::{Arc, atomic::AtomicUsize},
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crap_cms::{
    admin::{
        AdminState, custom_pages::CustomPageRegistry, server::build_router, templates,
        test_support::test_infra, translations::Translations,
    },
    config::{CrapConfig, EmailConfig, UploadConfig},
    core::{
        HookRef, Registry, SharedTokenProvider,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        collection::CollectionDefinition,
        email::create_email_provider,
        field::{FieldDefinition, FieldType},
        rate_limit::LoginRateLimiter,
        upload::{CollectionUpload, create_storage},
    },
    db::{migrate, pool},
    hooks::lifecycle::HookRunner,
};

const SECRET: &str = "test-jwt-secret";
const CSRF: &str = "test-csrf-token-12345";
const BOUNDARY: &str = "----CrapTestBoundary";

/// `access.png_only`: allows a create whose data is a PNG file — a rule that
/// reads a column the server derives from the file.
const PNG_ONLY_RULE: &str = "return function(ctx)\n    \
    return ctx.data ~= nil and ctx.data.mime_type == \"image/png\"\nend\n";

struct TestApp {
    tmp: TempDir,
    router: Router,
}

/// The shared state over `def`, with `access/png_only.lua` in the config dir.
fn state(tmp: &Path, def: CollectionDefinition) -> AdminState {
    fs::create_dir_all(tmp.join("access")).unwrap();
    fs::write(tmp.join("access/png_only.lua"), PNG_ONLY_RULE).unwrap();

    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = SECRET.into();
    config.admin.require_auth = false;
    config.access.default_deny = false;

    let db_pool = pool::create_pool(tmp, &config).expect("create pool");

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def);
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(tmp)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    let translations = Arc::new(Translations::load(tmp));
    let handlebars =
        templates::create_handlebars(tmp, false, translations.clone(), None).expect("handlebars");

    let storage = create_storage(tmp, &UploadConfig::default()).unwrap();
    let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new(SECRET));
    let infra = test_infra(
        db_pool,
        registry,
        hook_runner,
        storage,
        token_provider,
        &config,
        tmp,
    );

    AdminState {
        mcp_sessions: Arc::default(),
        infra,
        config,
        config_dir: tmp.to_path_buf(),
        handlebars,
        jwt_secret: SECRET.into(),
        email_provider: create_email_provider(&EmailConfig::default()).unwrap(),
        login_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_login_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        forgot_password_limiter: Arc::new(LoginRateLimiter::new(3, 900)),
        ip_forgot_password_limiter: Arc::new(LoginRateLimiter::new(20, 900)),
        mfa_limiter: Arc::new(LoginRateLimiter::new(5, 300)),
        ip_mfa_limiter: Arc::new(LoginRateLimiter::new(20, 300)),
        has_auth: false,
        translations,
        sse_connections: Arc::new(AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: CancellationToken::new(),
        password_provider: Arc::new(Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        custom_pages: CustomPageRegistry::default(),
    }
}

/// An upload collection `media` whose `create` rule is `access.png_only`.
fn setup() -> TestApp {
    let mut def = CollectionDefinition::new("media");
    def.upload = Some(CollectionUpload::new());
    def.access.create = Some(HookRef::new("access.png_only"));
    def.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text).build(),
        FieldDefinition::builder("mime_type", FieldType::Text).build(),
        FieldDefinition::builder("filesize", FieldType::Number).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
    ];

    let tmp = tempfile::tempdir().expect("tempdir");
    let router = build_router(state(tmp.path(), def));

    TestApp { tmp, router }
}

fn tiny_png() -> Vec<u8> {
    let mut buf = Vec::new();
    PngEncoder::new(&mut buf)
        .write_image(&[0u8, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)
        .unwrap();

    buf
}

/// A multipart body carrying one `_file` part.
fn multipart(filename: &str, content_type: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();

    body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"_file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    body
}

/// An anonymous `POST /api/upload/media` of one file.
async fn upload(app: &TestApp, filename: &str, content_type: &str, data: &[u8]) -> StatusCode {
    app.router
        .clone()
        .oneshot(
            Request::post("/api/upload/media")
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .header("Cookie", format!("crap_csrf={CSRF}"))
                .header("X-CSRF-Token", CSRF)
                .body(Body::from(multipart(filename, content_type, data)))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// How many files sit under `dir`, at any depth.
fn files_under(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };

    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();

            if path.is_dir() { files_under(&path) } else { 1 }
        })
        .sum()
}

/// Regression: the REST handler judged the collection's `create` rule before
/// reading the request, with no data at all — so a rule reading
/// `ctx.data.mime_type` refused every upload, the ones it allows included.
/// The rule is now judged once, by the service, on the request together with
/// the file's own columns.
#[tokio::test]
async fn a_create_rule_on_the_file_type_allows_the_files_it_names() {
    let app = setup();

    assert_eq!(
        upload(&app, "a.png", "image/png", &tiny_png()).await,
        StatusCode::CREATED
    );
}

/// The same rule refuses a file it does not name, and the refused file is
/// never stored.
#[tokio::test]
async fn a_create_rule_on_the_file_type_refuses_other_files_unstored() {
    let app = setup();

    assert_eq!(
        upload(&app, "notes.txt", "text/plain", b"plain text").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(files_under(&app.tmp.path().join("uploads")), 0);
}
