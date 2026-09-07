//! The shared-field (locale-lock) guard: a non-default-locale write that
//! carries a field which only exists under the default locale is rejected on
//! every surface and at every stage — the caller's input, a hook's injection,
//! collections and globals alike — never silently dropped.

use std::path::PathBuf;
use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{CollectionDefinition, GlobalDefinition, Hooks};
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::{DocumentFields, HookRef, Registry};
use crap_cms::db::{DbPool, LocaleContext, LocaleMode, migrate, pool};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    ServiceContext, ServiceError, WriteInput, create_document, update_document,
    update_global_document,
};
use serde_json::json;

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    registry: Arc<Registry>,
    locale: LocaleConfig,
}

fn articles_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("articles");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build(),
        FieldDefinition::builder("slug", FieldType::Text).build(),
    ];
    def.hooks = Hooks {
        before_change: vec![HookRef::new("hooks.inject.set_slug")],
        ..Default::default()
    };
    def
}

fn settings_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.fields = vec![
        FieldDefinition::builder("welcome_text", FieldType::Text)
            .localized(true)
            .build(),
        FieldDefinition::builder("max_items", FieldType::Number).build(),
    ];
    def
}

fn setup() -> Harness {
    let locale = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: true,
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.locale = locale.clone();

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(articles_def());
        reg.register_global(settings_def());
    }
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &locale).expect("sync");

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/locale_lock");
    let runner = HookRunner::builder()
        .config_dir(&fixture)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    Harness {
        _tmp: tmp,
        pool,
        runner,
        registry,
        locale,
    }
}

fn locale_ctx(h: &Harness, locale: &str) -> LocaleContext {
    LocaleContext {
        mode: LocaleMode::Single(locale.to_string()),
        config: h.locale.clone(),
    }
}

fn fields(pairs: &[(&str, serde_json::Value)]) -> DocumentFields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn assert_rejects_field(result: Result<(), ServiceError>, field: &str) {
    match result {
        Err(ServiceError::Validation(ve)) => {
            assert!(
                ve.errors.iter().any(|e| e.field == field),
                "expected a validation error on '{field}', got {ve:?}"
            );
        }
        other => panic!("expected a validation error on '{field}', got {other:?}"),
    }
}

/// Globals: a non-default-locale update carrying a shared field is rejected
/// like a collection update, not silently skipped at the DB edge.
#[test]
fn global_update_rejects_a_shared_field_under_a_non_default_locale() {
    let h = setup();
    let def = h.registry.get_global("settings").unwrap().clone();
    let ctx = ServiceContext::global("settings", &def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build();

    let en = locale_ctx(&h, "en");
    update_global_document(
        &ctx,
        WriteInput::builder(fields(&[
            ("welcome_text", json!("Hello")),
            ("max_items", json!(5)),
        ]))
        .locale_ctx(Some(&en))
        .build(),
    )
    .expect("default-locale write carries shared fields");

    let de = locale_ctx(&h, "de");
    let result = update_global_document(
        &ctx,
        WriteInput::builder(fields(&[("max_items", json!(10))]))
            .locale_ctx(Some(&de))
            .build(),
    );
    assert_rejects_field(result.map(|_| ()), "max_items");

    let (doc, _) = update_global_document(
        &ctx,
        WriteInput::builder(fields(&[("welcome_text", json!("Hallo"))]))
            .locale_ctx(Some(&de))
            .build(),
    )
    .expect("a localized-only translation write succeeds");
    assert_eq!(doc.get_str("welcome_text"), Some("Hallo"));
}

/// A `before_change` hook that injects a shared field into a non-default
/// locale write is caught at persist time — the write fails loudly instead of
/// succeeding minus the injected field.
#[test]
fn hook_injected_shared_field_is_rejected_not_dropped() {
    let h = setup();
    let def = h.registry.get_collection("articles").unwrap().clone();
    let ctx = ServiceContext::collection("articles", &def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build();

    let en = locale_ctx(&h, "en");
    let (doc, _) = create_document(
        &ctx,
        WriteInput::builder(fields(&[("title", json!("Hello"))]))
            .locale_ctx(Some(&en))
            .build(),
    )
    .expect("create under the default locale");
    assert_eq!(
        doc.get_str("slug"),
        Some("injected"),
        "the hook's injected shared field lands under the default locale"
    );

    let de = locale_ctx(&h, "de");
    let result = update_document(
        &ctx,
        &doc.id,
        WriteInput::builder(fields(&[("title", json!("Hallo"))]))
            .locale_ctx(Some(&de))
            .build(),
    );
    assert_rejects_field(result.map(|_| ()), "slug");
}
