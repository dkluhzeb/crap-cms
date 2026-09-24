//! The localized-completeness (`required_locales`) gate judges the document's
//! POST-write state, not the translations the write is about to replace.
//!
//! Two writes land more than the locale they target: publishing a pending draft
//! writes the draft's other locales back over the row, and restoring a version
//! writes every locale from its snapshot. Judging the live row let the first
//! publish an empty required translation, and made the second refuse a complete
//! snapshot because the row it replaces was incomplete.

use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{CollectionDefinition, GlobalDefinition, VersionsConfig};
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::{DocumentFields, Registry, RequiredLocales};
use crap_cms::db::{DbPool, LocaleContext, LocaleMode, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    ServiceContext, ServiceError, UpdateManyOptions, WriteInput, restore_collection_version,
    update_document, update_global_document, update_many,
};
use serde_json::{Value, json};

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
    global: GlobalDefinition,
    locale: LocaleConfig,
}

/// A localized field that must carry a value in EVERY configured locale.
fn required_everywhere(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text)
        .localized(true)
        .required(true)
        .required_locales(RequiredLocales::All)
        .build()
}

fn pages_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.timestamps = true;
    def.fields = vec![required_everywhere("title")];
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn settings_def() -> GlobalDefinition {
    let mut def = GlobalDefinition::new("settings");
    def.fields = vec![required_everywhere("welcome_text")];
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
    let def = pages_def();
    let global = settings_def();

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        reg.register_collection(def.clone());
        reg.register_global(global.clone());
    }
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
        global,
        locale,
    }
}

fn locale_ctx(h: &Harness, locale: &str) -> LocaleContext {
    LocaleContext {
        mode: LocaleMode::Single(locale.to_string()),
        config: h.locale.clone(),
    }
}

fn fields(pairs: &[(&str, Value)]) -> DocumentFields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn page_ctx(h: &Harness) -> ServiceContext<'_> {
    ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build()
}

fn global_ctx(h: &Harness) -> ServiceContext<'_> {
    ServiceContext::global("settings", &h.global)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build()
}

/// Seed a page straight through the query layer, so a deliberately incomplete
/// row can exist without the write gate this file is about rejecting it.
fn seed_page(h: &Harness, en: &str, de: Option<&str>) -> String {
    let conn = h.pool.get().unwrap();

    let en_ctx = locale_ctx(h, "en");
    let doc = query::create(
        &conn,
        "pages",
        &h.def,
        &fields(&[("title", json!(en))]),
        Some(&en_ctx),
    )
    .expect("create");
    let id = doc.id.to_string();

    if let Some(de) = de {
        let de_ctx = locale_ctx(h, "de");
        query::update(
            &conn,
            "pages",
            &h.def,
            &id,
            &fields(&[("title", json!(de))]),
            Some(&de_ctx),
        )
        .expect("german title");
    }

    id
}

/// Seed the global with both translations, the same way.
fn seed_global(h: &Harness) {
    let conn = h.pool.get().unwrap();

    for (locale, text) in [("en", "Hello"), ("de", "Hallo")] {
        let ctx = locale_ctx(h, locale);
        query::update_global(
            &conn,
            "settings",
            &h.global,
            &fields(&[("welcome_text", json!(text))]),
            Some(&ctx),
        )
        .expect("seed the global");
    }
}

fn title_in(h: &Harness, id: &str, locale: &str) -> Option<String> {
    let conn = h.pool.get().unwrap();

    query::find_by_id(&conn, "pages", &h.def, id, Some(&locale_ctx(h, locale)))
        .unwrap()
        .and_then(|d| d.get_str("title").map(str::to_string))
}

/// Save a draft of the page under `locale`.
fn draft_page(h: &Harness, id: &str, locale: &str, title: &str) {
    let ctx = page_ctx(h);
    let lctx = locale_ctx(h, locale);

    update_document(
        &ctx,
        id,
        WriteInput::builder(fields(&[("title", json!(title))]))
            .locale_ctx(Some(&lctx))
            .draft(true)
            .build(),
    )
    .expect("draft save");
}

fn assert_required_locale<T>(
    result: std::result::Result<T, ServiceError>,
    field: &str,
    locale: &str,
) {
    match result {
        Err(ServiceError::Validation(ve)) => assert!(
            ve.errors.iter().any(|e| {
                e.field == field
                    && e.key.as_deref() == Some("validation.required_locale")
                    && e.params.get("locale").map(String::as_str) == Some(locale)
            }),
            "expected a required_locale error on '{field}' for '{locale}', got {ve:?}"
        ),
        Err(other) => panic!("expected a validation error on '{field}', got {other:?}"),
        Ok(_) => panic!("expected a validation error on '{field}' for '{locale}'"),
    }
}

/// A draft that clears a required translation cannot be published: the publish
/// writes that empty value back over the row, so the gate has to see it even
/// though the request targets another locale.
#[test]
fn publishing_a_draft_that_cleared_a_translation_is_rejected() {
    let h = setup();
    let id = seed_page(&h, "Hello", Some("Hallo"));

    draft_page(&h, &id, "de", "");

    let ctx = page_ctx(&h);
    let en = locale_ctx(&h, "en");
    let result = update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", json!("Hello v2"))]))
            .locale_ctx(Some(&en))
            .build(),
    );

    assert_required_locale(result.map(|_| ()), "title", "de");
    assert_eq!(
        title_in(&h, &id, "de").as_deref(),
        Some("Hallo"),
        "the refused publish left the stored translation alone"
    );
}

/// The mirror case: the draft SUPPLIES the translation the stored row lacks, so
/// the document is complete once the publish lands and must go through.
#[test]
fn publishing_a_draft_that_supplies_a_translation_succeeds() {
    let h = setup();
    let id = seed_page(&h, "Hello", None);

    draft_page(&h, &id, "de", "Hallo");

    let ctx = page_ctx(&h);
    let en = locale_ctx(&h, "en");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", json!("Hello v2"))]))
            .locale_ctx(Some(&en))
            .build(),
    )
    .expect("the draft completes the document");

    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Hello v2"));
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo"));
}

/// A bulk publish makes the same pending draft live, so it is gated the same
/// way — the single-document rule cannot be missing here.
#[test]
fn a_bulk_publish_is_gated_like_a_single_one() {
    let h = setup();
    let id = seed_page(&h, "Hello", Some("Hallo"));

    draft_page(&h, &id, "de", "");

    let ctx = ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .override_access(true)
        .build();
    let en = locale_ctx(&h, "en");

    let result = update_many(
        &ctx,
        &[],
        &fields(&[("title", json!("Hello v2"))]),
        &h.locale,
        &UpdateManyOptions::builder()
            .locale_ctx(Some(&en))
            .run_hooks(false)
            .build(),
    );

    assert_required_locale(result.map(|_| ()), "title", "de");
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo"));
}

/// Globals publish their pending draft as one unit too.
#[test]
fn a_global_publish_is_gated_like_a_collection() {
    let h = setup();
    seed_global(&h);

    let ctx = global_ctx(&h);
    let de = locale_ctx(&h, "de");
    update_global_document(
        &ctx,
        WriteInput::builder(fields(&[("welcome_text", json!(""))]))
            .locale_ctx(Some(&de))
            .draft(true)
            .build(),
    )
    .expect("draft save clears the German text");

    let en = locale_ctx(&h, "en");
    let result = update_global_document(
        &ctx,
        WriteInput::builder(fields(&[("welcome_text", json!("Hello v2"))]))
            .locale_ctx(Some(&en))
            .build(),
    );

    assert_required_locale(result.map(|_| ()), "welcome_text", "de");
}

/// A restore writes EVERY locale from its snapshot, so a complete snapshot
/// restores onto an incomplete row — the live translations are the ones it
/// replaces, not the ones it leaves behind.
#[test]
fn restoring_a_complete_snapshot_over_an_incomplete_row_succeeds() {
    let h = setup();
    let id = seed_page(&h, "Hello", None);

    let conn = h.pool.get().unwrap();
    let version = query::create_version(
        &conn,
        "pages",
        &id,
        "published",
        &json!({ "title": "Then", "title__en": "Then", "title__de": "Damals" }),
    )
    .expect("a version to restore");
    drop(conn);

    let ctx = page_ctx(&h);
    restore_collection_version(&ctx, &id, &version.id, &h.locale)
        .expect("a complete snapshot restores");

    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Then"));
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Damals"));
}

/// And the other direction: a snapshot missing a required translation is
/// refused, even when the row it would overwrite has one.
#[test]
fn restoring_a_snapshot_missing_a_translation_is_refused() {
    let h = setup();
    let id = seed_page(&h, "Hello", Some("Hallo"));

    let conn = h.pool.get().unwrap();
    let version = query::create_version(
        &conn,
        "pages",
        &id,
        "published",
        &json!({ "title": "Then", "title__en": "Then", "title__de": "" }),
    )
    .expect("a version to restore");
    drop(conn);

    let ctx = page_ctx(&h);
    let result = restore_collection_version(&ctx, &id, &version.id, &h.locale);

    assert_required_locale(result.map(|_| ()), "title", "de");
    assert_eq!(
        title_in(&h, &id, "en").as_deref(),
        Some("Hello"),
        "the refused restore changed nothing"
    );
}
