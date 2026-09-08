//! Versions on a localized collection: a snapshot records every locale's
//! value, so restoring one never wipes the other locales' translations, and a
//! draft read returns the content of the locale being read.

use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{CollectionDefinition, VersionsConfig};
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{DbPool, LocaleContext, LocaleMode, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    FindByIdInput, RunnerReadHooks, ServiceContext, WriteInput, find_document_by_id,
    restore_collection_version, update_document,
};
use serde_json::json;

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
    locale: LocaleConfig,
}

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build(),
        FieldDefinition::builder("slug", FieldType::Text).build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn setup() -> Harness {
    let locale = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback: false,
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.locale = locale.clone();

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let def = make_def();
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &locale).expect("sync");

    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    Harness {
        _tmp: tmp,
        pool,
        runner,
        def,
        locale,
    }
}

fn ctx_for(h: &Harness, locale: &str) -> LocaleContext {
    LocaleContext {
        mode: LocaleMode::Single(locale.to_string()),
        config: h.locale.clone(),
    }
}

fn fields(pairs: &[(&str, &str)]) -> DocumentFields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), json!(v)))
        .collect()
}

fn service_ctx(h: &Harness) -> ServiceContext<'_> {
    ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build()
}

/// Seed one document with an English and a German title.
fn seed(h: &Harness) -> String {
    let conn = h.pool.get().unwrap();
    let en = ctx_for(h, "en");
    let doc = query::create(
        &conn,
        "pages",
        &h.def,
        &fields(&[("title", "Hello"), ("slug", "hello")]),
        Some(&en),
    )
    .expect("create");

    let de = ctx_for(h, "de");
    query::update(
        &conn,
        "pages",
        &h.def,
        &doc.id,
        &fields(&[("title", "Hallo")]),
        Some(&de),
    )
    .expect("german title");

    doc.id.to_string()
}

fn title_in(h: &Harness, id: &str, locale: &str) -> Option<String> {
    let conn = h.pool.get().unwrap();
    query::find_by_id(&conn, "pages", &h.def, id, Some(&ctx_for(h, locale)))
        .unwrap()
        .and_then(|d| d.get_str("title").map(str::to_string))
}

/// A published update made under one locale snapshots BOTH locales, so
/// restoring it leaves the other locale's translation intact.
#[test]
fn restoring_a_version_keeps_every_locale() {
    let h = setup();
    let id = seed(&h);

    let ctx = service_ctx(&h);
    let de = ctx_for(&h, "de");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Hallo v2")]))
            .locale_ctx(Some(&de))
            .build(),
    )
    .expect("german update");

    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Hello"));
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo v2"));

    let conn = h.pool.get().unwrap();
    let versions = query::list_versions(&conn, "pages", &id, false, None, None).expect("versions");
    let latest = versions.first().expect("a version exists");
    drop(conn);

    restore_collection_version(&ctx, &id, &latest.id, &h.locale).expect("restore");

    assert_eq!(
        title_in(&h, &id, "en").as_deref(),
        Some("Hello"),
        "restoring a German-made version must not wipe the English title"
    );
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo v2"));
}

/// A draft saved under one locale is read back per locale, not served as
/// every locale's content.
#[test]
fn a_draft_reads_per_locale() {
    let h = setup();
    let id = seed(&h);

    let ctx = service_ctx(&h);
    let en = ctx_for(&h, "en");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Hello draft")]))
            .locale_ctx(Some(&en))
            .draft(true)
            .build(),
    )
    .expect("english draft");

    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, None, None);
    let read_ctx = ServiceContext::collection("pages", &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .locale_config(Some(&h.locale))
        .build();

    let de = ctx_for(&h, "de");
    let de_doc = find_document_by_id(
        &read_ctx,
        &FindByIdInput::builder(&id)
            .locale_ctx(Some(&de))
            .use_draft(true)
            .build(),
    )
    .expect("read the draft under de")
    .expect("document");

    assert_eq!(
        de_doc.get_str("title"),
        Some("Hallo"),
        "an English draft must not surface as the German title"
    );
}
