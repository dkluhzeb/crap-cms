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
    FindByIdInput, OpDeadline, RunnerReadHooks, ServiceContext, UpdateManyOptions, WriteInput,
    find_document_by_id, restore_collection_version, update_document, update_many,
};
use serde_json::json;

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
    locale: LocaleConfig,
}

fn slide_fields() -> Vec<FieldDefinition> {
    vec![FieldDefinition::builder("caption", FieldType::Text).build()]
}

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("pages");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build(),
        FieldDefinition::builder("slug", FieldType::Text).build(),
        FieldDefinition::builder("slides", FieldType::Array)
            .localized(true)
            .fields(slide_fields())
            .build(),
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

    // The other half, and the one that was silently broken: reading the draft
    // under the locale it was WRITTEN in must show the edit. The snapshot
    // records every locale's decorated column straight from the main table,
    // which a draft save never writes — so without stamping the edit into the
    // writing locale's column, resolving from those columns handed back the
    // published text and the draft looked like it had never been saved.
    let en_doc = find_document_by_id(
        &read_ctx,
        &FindByIdInput::builder(&id)
            .locale_ctx(Some(&en))
            .use_draft(true)
            .build(),
    )
    .expect("read the draft under en")
    .expect("document");

    assert_eq!(
        en_doc.get_str("title"),
        Some("Hello draft"),
        "the draft must be visible under the locale it was written in"
    );

    // And the response carries no decorated columns — a shape no other read
    // produces, and one that would hand a `de` reader the `en` translation.
    for key in en_doc.fields.keys() {
        assert!(
            !key.contains("__"),
            "a per-locale column leaked into the read: {key}"
        );
    }
}

/// A draft save reports the draft it stored, resolved for the writing locale
/// and with no per-locale columns riding along.
#[test]
fn a_draft_save_reports_the_draft_for_its_own_locale() {
    let h = setup();
    let id = seed(&h);

    let ctx = service_ctx(&h);
    let de = ctx_for(&h, "de");
    let (doc, _) = update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Hallo Entwurf")]))
            .locale_ctx(Some(&de))
            .draft(true)
            .build(),
    )
    .expect("german draft");

    assert_eq!(doc.get_str("title"), Some("Hallo Entwurf"));
    assert_eq!(doc.get_str("_status"), Some("draft"));

    for key in doc.fields.keys() {
        assert!(
            !key.contains("__"),
            "a per-locale column leaked into the write response: {key}"
        );
    }

    // The published row is untouched by a draft save.
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo"));
}

/// A BULK update on a localized collection snapshots every locale too. The
/// bulk path built its snapshot without the locale config, so the version held
/// one value per localized field and restoring it wiped every other
/// translation.
#[test]
fn restoring_a_bulk_update_version_keeps_every_locale() {
    let h = setup();
    let id = seed(&h);

    let en = ctx_for(&h, "en");
    let ctx = ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .override_access(true)
        .build();

    update_many(
        &ctx,
        &[],
        &fields(&[("title", "Hello v2")]),
        &h.locale,
        &UpdateManyOptions {
            locale_ctx: Some(&en),
            run_hooks: false,
            draft: false,
            ui_locale: None,
            max_documents: 0,
            deadline: OpDeadline::none(),
        },
    )
    .expect("bulk update");

    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Hello v2"));
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo"));

    let conn = h.pool.get().unwrap();
    let versions = query::list_versions(&conn, "pages", &id, false, None, None).expect("versions");
    let latest = versions.first().expect("the bulk update created a version");
    drop(conn);

    restore_collection_version(&ctx, &id, &latest.id, &h.locale).expect("restore");

    assert_eq!(
        title_in(&h, &id, "de").as_deref(),
        Some("Hallo"),
        "restoring a bulk-update version must not wipe the German title"
    );
    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Hello v2"));
}

/// Captions of one locale's `slides` rows, read straight from the join table.
fn slides_in(h: &Harness, id: &str, locale: &str) -> Vec<String> {
    let conn = h.pool.get().unwrap();

    query::find_array_rows(&conn, "pages", "slides", id, &slide_fields(), Some(locale))
        .unwrap()
        .iter()
        .filter_map(|row| {
            row.get("caption")
                .and_then(|c| c.as_str())
                .map(str::to_string)
        })
        .collect()
}

/// Captions of `slides` in a document returned by a read.
fn captions(doc: &crap_cms::core::Document) -> Vec<String> {
    doc.fields
        .get("slides")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    r.get("caption")
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn slides(captions: &[&str]) -> DocumentFields {
    let rows: Vec<serde_json::Value> = captions.iter().map(|c| json!({ "caption": c })).collect();

    [("slides".to_string(), json!(rows))].into_iter().collect()
}

fn write(h: &Harness, id: &str, locale: &str, data: DocumentFields, draft: bool) {
    let ctx = service_ctx(h);
    let locale_ctx = ctx_for(h, locale);

    update_document(
        &ctx,
        id,
        WriteInput::builder(data)
            .locale_ctx(Some(&locale_ctx))
            .draft(draft)
            .build(),
    )
    .expect("write");
}

fn read_draft(h: &Harness, id: &str, locale: &str) -> crap_cms::core::Document {
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, None, None);
    let read_ctx = ServiceContext::collection("pages", &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .locale_config(Some(&h.locale))
        .build();
    let locale_ctx = ctx_for(h, locale);

    find_document_by_id(
        &read_ctx,
        &FindByIdInput::builder(id)
            .locale_ctx(Some(&locale_ctx))
            .use_draft(true)
            .build(),
    )
    .expect("read")
    .expect("document")
}

fn restore_latest(h: &Harness, id: &str) {
    restore_nth_newest(h, id, 0);
}

/// Restore the version at `index` in newest-first order (0 = latest).
fn restore_nth_newest(h: &Harness, id: &str, index: usize) {
    let conn = h.pool.get().unwrap();
    let versions = query::list_versions(&conn, "pages", id, false, None, None).expect("versions");
    let version = versions.get(index).expect("that version").id.clone();
    drop(conn);

    restore_collection_version(&service_ctx(h), id, &version, &h.locale).expect("restore");
}

/// A snapshot keeps each locale's rows of a localized array apart, and a
/// restore writes them back to their own locale — not all into the default.
/// The German rows change after the snapshot, so the restore has real work to
/// do: re-create the snapshot's German row and drop the later one.
#[test]
fn restoring_keeps_each_locales_array_rows() {
    let h = setup();
    let id = seed(&h);

    write(&h, &id, "en", slides(&["A", "B"]), false);
    write(&h, &id, "de", slides(&["X"]), false);
    write(&h, &id, "de", slides(&["Y"]), false);

    // Newest first: [de Y, de X, en A B] → restore the `de X` version.
    restore_nth_newest(&h, &id, 1);

    assert_eq!(slides_in(&h, &id, "en"), ["A", "B"]);
    assert_eq!(slides_in(&h, &id, "de"), ["X"]);
}

/// A draft of a localized array saved under `de` changes only the German rows:
/// an English draft read still shows the English rows, and restoring the draft
/// version leaves the English rows in place.
#[test]
fn a_german_draft_of_localized_rows_leaves_english_rows_alone() {
    let h = setup();
    let id = seed(&h);

    write(&h, &id, "en", slides(&["A", "B"]), false);
    write(&h, &id, "de", slides(&["X"]), false);
    write(&h, &id, "de", slides(&["X2"]), true);

    assert_eq!(captions(&read_draft(&h, &id, "en")), ["A", "B"]);
    assert_eq!(captions(&read_draft(&h, &id, "de")), ["X2"]);

    restore_latest(&h, &id);

    assert_eq!(slides_in(&h, &id, "en"), ["A", "B"]);
    assert_eq!(slides_in(&h, &id, "de"), ["X2"]);
}

/// A draft saved under a second locale builds on the pending draft, so the
/// first locale's draft edit survives.
#[test]
fn a_second_locales_draft_keeps_the_first_locales_draft_edit() {
    let h = setup();
    let id = seed(&h);

    write(&h, &id, "en", fields(&[("title", "Hello draft")]), true);
    write(&h, &id, "de", fields(&[("title", "Hallo Entwurf")]), true);

    assert_eq!(
        read_draft(&h, &id, "en").get_str("title"),
        Some("Hello draft")
    );
    assert_eq!(
        read_draft(&h, &id, "de").get_str("title"),
        Some("Hallo Entwurf")
    );
}

/// A partial draft update (one field, as an API client sends it) keeps the
/// earlier draft edits of every other field.
#[test]
fn a_partial_draft_update_keeps_earlier_draft_edits() {
    let h = setup();
    let id = seed(&h);

    write(&h, &id, "en", fields(&[("title", "Hello draft")]), true);
    write(&h, &id, "en", fields(&[("slug", "draft-slug")]), true);

    let draft = read_draft(&h, &id, "en");
    assert_eq!(draft.get_str("title"), Some("Hello draft"));
    assert_eq!(draft.get_str("slug"), Some("draft-slug"));
}
