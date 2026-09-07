//! Crate-internal `#[cfg(test)]` builders for a minimal in-memory [`AdminState`],
//! shared by the unit tests across `src/admin/**` that need a real state (DB,
//! registry, translations) to drive a handler helper. Integration tests in
//! `tests/admin_*.rs` build their own state via [`test_support`](super::test_support).

use std::sync::{Arc, atomic::AtomicUsize};

use r2d2_sqlite::SqliteConnectionManager;
use tokio_util::sync::CancellationToken;

use crate::{
    admin::{AdminState, Translations, custom_pages::CustomPageRegistry},
    config::{CrapConfig, EmailConfig, UploadConfig},
    core::{
        Registry, SharedTokenProvider,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        email::create_email_provider,
        rate_limit::LoginRateLimiter,
        upload::create_storage,
    },
    db::DbPool,
    hooks::HookRunner,
};

/// Minimal state with an empty registry and `default_deny = false`.
pub(crate) fn test_admin_state() -> AdminState {
    test_admin_state_full(false, Registry::default())
}

/// Like [`test_admin_state`] but with a configurable `access.default_deny`, so a
/// test can exercise the access-gated label reads (deny → labels filtered out).
pub(crate) fn test_admin_state_with_deny(default_deny: bool) -> AdminState {
    test_admin_state_full(default_deny, Registry::default())
}

/// Like [`test_admin_state`] but seeded with a pre-built [`Registry`], so a test
/// can enrich a relationship/upload field whose target collection must be
/// resolvable from `state.infra.registry`.
pub(crate) fn test_admin_state_with_registry(registry: Registry) -> AdminState {
    test_admin_state_full(false, registry)
}

fn test_admin_state_full(default_deny: bool, registry: Registry) -> AdminState {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SqliteConnectionManager::memory();
    let pool = DbPool::from_pool(r2d2::Pool::builder().max_size(4).build(manager).unwrap());
    let registry: Arc<Registry> = Arc::new(registry);
    let mut config = CrapConfig::test_default();
    config.access.default_deny = default_deny;
    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();
    let hbs = Arc::new(handlebars::Handlebars::new());
    let translations = Arc::new(Translations::load(tmp.path()));
    let storage = create_storage(tmp.path(), &UploadConfig::default()).unwrap();
    let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new("test-secret"));
    let infra = crate::admin::test_support::test_infra(
        pool,
        Arc::clone(&registry),
        hook_runner,
        storage,
        token_provider,
        &config,
        tmp.path(),
    );

    AdminState {
        mcp_sessions: Arc::default(),
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        handlebars: hbs,
        jwt_secret: "test".into(),
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
