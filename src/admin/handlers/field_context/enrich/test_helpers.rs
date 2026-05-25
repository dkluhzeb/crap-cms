//! Shared `#[cfg(test)]` fixtures for the `enrich/*` test modules.
//!
//! The wrappers convert the typed enrichment-API output to `serde_json::Value`
//! so existing JSON-style assertions (which test the wire shape consumed by
//! templates) keep working. [`make_test_state`] builds a minimal in-memory
//! [`AdminState`] for tests that need DB access.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::AtomicUsize},
};

use r2d2_sqlite::SqliteConnectionManager;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    admin::{
        AdminState, Translations,
        context::field::{FieldContext, RichtextField},
        custom_pages::CustomPageRegistry,
        handlers::field_context::{builder::build_field_contexts, enrich},
    },
    config::{CrapConfig, EmailConfig, UploadConfig},
    core::{
        DocumentFields, FieldDefinition, FieldType, Registry,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        email::{EmailRenderer, create_email_provider},
        event::InProcessInvalidationBus,
        rate_limit::LoginRateLimiter,
        upload::create_storage,
    },
    db::{DbConnection, DbPool, query::LocaleContext, query::Singleflight},
    hooks::HookRunner,
};

use super::{EnrichOptions, SubFieldOpts, types};

/// Build a minimal [`FieldDefinition`] with default admin/validation settings.
pub(super) fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, ft).build()
}

/// Run [`build_field_contexts`] and serialize each typed result to its wire
/// JSON. Mirrors the parent module's `test_helpers::build_value_contexts` —
/// duplicated here because that helper is `pub(super)`-scoped to its parent
/// module.
pub(super) fn build_value_contexts(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<Value> {
    build_field_contexts(fields, values, errors, filter_hidden, non_default_locale)
        .into_iter()
        .map(|fc| fc.to_value())
        .collect()
}

/// Test wrapper for [`super::build_enriched_sub_field_context`] returning
/// `Value` so existing JSON-style assertions keep working.
pub(super) fn build_enriched_sub_field_value(
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    parent_name: &str,
    idx: usize,
    opts: &SubFieldOpts,
) -> Value {
    super::build_enriched_sub_field_context(sf, raw_value, parent_name, idx, opts).to_value()
}

/// Test wrapper for [`super::enrich_field_contexts`] taking `&mut Vec<Value>`.
/// Deserializes at entry, calls the production fn, reserializes at exit.
pub(super) fn enrich_field_contexts_values(
    fields: &mut Vec<Value>,
    field_defs: &[FieldDefinition],
    doc_fields: &DocumentFields,
    state: &AdminState,
    opts: &EnrichOptions,
) {
    let mut typed: Vec<FieldContext> = fields
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("test field-context must deserialize"))
        .collect();
    enrich::enrich_field_contexts(&mut typed, field_defs, doc_fields, state, opts);
    *fields = typed.into_iter().map(|fc| fc.to_value()).collect();
}

/// Test wrapper for [`super::enrich_nested_fields`] taking `&mut Vec<Value>`.
pub(super) fn enrich_nested_fields_values(
    sub_fields: &mut Vec<Value>,
    field_defs: &[FieldDefinition],
    conn: &dyn DbConnection,
    reg: &Registry,
    rel_locale_ctx: Option<&LocaleContext>,
) {
    let mut typed: Vec<FieldContext> = sub_fields
        .iter()
        .map(|v| serde_json::from_value(v.clone()).expect("test sub-field must deserialize"))
        .collect();
    super::enrich_nested_fields(&mut typed, field_defs, conn, reg, rel_locale_ctx);
    *sub_fields = typed.into_iter().map(|fc| fc.to_value()).collect();
}

/// Test wrapper for `types::enrich_richtext` taking `&mut Value`.
pub(super) fn enrich_richtext_value(ctx: &mut Value, reg: &Registry) {
    let mut typed: RichtextField =
        serde_json::from_value(ctx.clone()).expect("test richtext ctx must deserialize");
    types::enrich_richtext(&mut typed, reg);
    *ctx = serde_json::to_value(typed).expect("RichtextField serializes infallibly");
}

/// Build a minimal [`AdminState`] backed by an in-memory `SQLite` pool. Used by
/// tests that exercise DB-touching enrichment paths.
pub(super) fn make_test_state() -> AdminState {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SqliteConnectionManager::memory();
    let pool = DbPool::from_pool(r2d2::Pool::builder().max_size(4).build(manager).unwrap());
    let registry: Arc<Registry> = Arc::new(Registry::default());
    let config = CrapConfig::default();
    let hook_runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .unwrap();
    let hbs = Arc::new(handlebars::Handlebars::new());
    let email_renderer = Arc::new(EmailRenderer::new(tmp.path()).unwrap());
    let login_limiter = Arc::new(LoginRateLimiter::new(5, 300));
    let ip_login_limiter = Arc::new(LoginRateLimiter::new(20, 300));
    let translations = Arc::new(Translations::load(tmp.path()));

    AdminState {
        config,
        config_dir: tmp.path().to_path_buf(),
        pool,
        registry,
        handlebars: hbs,
        hook_runner,
        jwt_secret: "test".into(),
        email_renderer,
        email_provider: create_email_provider(&EmailConfig::default()).unwrap(),
        event_transport: None,
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter: Arc::new(LoginRateLimiter::new(3, 900)),
        ip_forgot_password_limiter: Arc::new(LoginRateLimiter::new(20, 900)),
        has_auth: false,
        translations,
        sse_connections: Arc::new(AtomicUsize::new(0)),
        max_sse_connections: 0,
        shutdown: CancellationToken::new(),
        storage: create_storage(tmp.path(), &UploadConfig::default()).unwrap(),
        token_provider: Arc::new(JwtTokenProvider::new("test-secret")),
        password_provider: Arc::new(Argon2PasswordProvider),
        subscriber_send_timeout_ms: 1000,
        invalidation_transport: Arc::new(InProcessInvalidationBus::new()),
        populate_singleflight: Arc::new(Singleflight::new()),
        cache: None,
        custom_pages: CustomPageRegistry::default(),
    }
}
