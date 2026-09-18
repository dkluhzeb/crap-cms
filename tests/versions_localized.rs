//! Versions on a localized collection: a snapshot records every locale's
//! value, so restoring one never wipes the other locales' translations, and a
//! draft read returns the content of the locale being read.

use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{CollectionDefinition, VersionsConfig};
use crap_cms::core::field::{FieldAccess, FieldDefinition, FieldType};
use crap_cms::core::{DocumentFields, HookRef, Registry};
use crap_cms::db::{DbPool, LocaleContext, LocaleMode, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    FindByIdInput, ListVersionsInput, OpDeadline, RunnerReadHooks, ServiceContext,
    UpdateManyOptions, WriteInput, find_document_by_id, list_versions, restore_collection_version,
    unpublish_document, update_document, update_many,
};
use serde_json::{Value, json};

struct Harness {
    tmp: tempfile::TempDir,
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
        tmp,
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

/// The `title` of the newest version snapshot, read under `locale`
/// (`None` = unqualified, `Some("all")` = every locale at once).
fn latest_version_title(h: &Harness, id: &str, locale: Option<&str>) -> Value {
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, None, None);
    let read_ctx = ServiceContext::collection("pages", &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .locale_config(Some(&h.locale))
        .build();

    let locale_ctx =
        LocaleContext::from_locale_string(locale, &h.locale).expect("a configured locale");
    let input = ListVersionsInput::builder(id)
        .locale_ctx(locale_ctx.as_ref())
        .build();

    let mut listed = list_versions(&read_ctx, &input).expect("version history");
    let newest = listed.docs.remove(0);

    newest.snapshot.get("title").cloned().expect("a title")
}

/// A version snapshot reads as a document in the locale the caller asks for:
/// the German values under `de`, every locale's value under `all`, and the
/// default locale when no locale is given. Without the locale the history
/// surface was pinned to the default locale and a snapshot's translations
/// were unreachable.
#[test]
fn a_version_snapshot_reads_in_the_requested_locale() {
    let h = setup();
    let id = seed(&h);

    write(&h, &id, "de", fields(&[("title", "Hallo v2")]), false);

    assert_eq!(latest_version_title(&h, &id, None), json!("Hello"));
    assert_eq!(latest_version_title(&h, &id, Some("de")), json!("Hallo v2"));
    assert_eq!(
        latest_version_title(&h, &id, Some("all")),
        json!({ "en": "Hello", "de": "Hallo v2" })
    );
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

/// Regression: a draft save reported — to `after_change` hooks, the caller and
/// the event — the published rows of its array fields: hydrating the stored row
/// over the draft replaced the draft's rows.
#[test]
fn a_draft_save_reports_its_own_rows() {
    let h = setup();
    let id = seed(&h);
    write(&h, &id, "en", slides(&["published"]), false);

    let ctx = service_ctx(&h);
    let locale_ctx = ctx_for(&h, "en");
    let (doc, _) = update_document(
        &ctx,
        &id,
        WriteInput::builder(slides(&["drafted"]))
            .locale_ctx(Some(&locale_ctx))
            .draft(true)
            .build(),
    )
    .expect("draft save");

    assert_eq!(captions(&doc), vec!["drafted".to_string()]);
}

/// Regression: a restore reported the document as read before its rows and
/// status were restored — without its array rows, and with the pre-restore
/// status — and published that as the restore event. A restore takes the
/// snapshot's own status, so restoring a published version onto an unpublished
/// document reports it published.
#[test]
fn a_restore_reports_the_restored_document() {
    let h = setup();
    let id = seed(&h);
    write(&h, &id, "en", slides(&["first"]), false);
    write(&h, &id, "en", slides(&["second"]), false);
    unpublish_document(&service_ctx(&h), &id).expect("unpublish");

    // Newest first: the unpublish, the second write, then the first.
    let conn = h.pool.get().unwrap();
    let versions = query::list_versions(&conn, "pages", &id, false, None, None).expect("versions");
    let version = versions.get(2).expect("the first write's version");
    assert_eq!(version.status, "published");
    let version = version.id.clone();
    drop(conn);

    let doc =
        restore_collection_version(&service_ctx(&h), &id, &version, &h.locale).expect("restore");

    assert_eq!(captions(&doc), vec!["first".to_string()]);
    assert_eq!(
        doc.get_str("_status"),
        Some("published"),
        "the restore reports the snapshot's status, not the pre-restore draft status"
    );
}

/// Regression: unpublish hydrated a localized array without a locale, so it
/// reported every locale's rows at once.
#[test]
fn unpublish_reports_the_default_locales_rows() {
    let h = setup();
    let id = seed(&h);
    write(&h, &id, "en", slides(&["english"]), false);
    write(&h, &id, "de", slides(&["deutsch"]), false);

    let doc = unpublish_document(&service_ctx(&h), &id).expect("unpublish");

    assert_eq!(captions(&doc), vec!["english".to_string()]);
}

/// The `slug` — a shared (non-localized) field, so it has one value whatever
/// locale it is read under.
fn slug_of(h: &Harness, id: &str) -> Option<String> {
    let conn = h.pool.get().unwrap();
    query::find_by_id(&conn, "pages", &h.def, id, Some(&ctx_for(h, "en")))
        .unwrap()
        .and_then(|d| d.get_str("slug").map(str::to_string))
}

/// A publish makes the pending draft live as ONE unit. A draft saved in German
/// used to be stranded in history when the document was published in English:
/// the publish adopted only the request locale's drafted values, and the new
/// published snapshot was rebuilt from the row, so nothing carried the German
/// draft forward.
#[test]
fn publishing_in_one_locale_publishes_every_locales_draft() {
    let h = setup();
    let id = seed(&h);
    let ctx = service_ctx(&h);

    let de = ctx_for(&h, "de");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Hallo Entwurf")]))
            .locale_ctx(Some(&de))
            .draft(true)
            .build(),
    )
    .expect("german draft");

    let en = ctx_for(&h, "en");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("slug", "drafted-slug")]))
            .locale_ctx(Some(&en))
            .draft(true)
            .build(),
    )
    .expect("english draft");

    // The publish sends only the English title.
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Hello published")]))
            .locale_ctx(Some(&en))
            .build(),
    )
    .expect("publish");

    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Hello published"));
    assert_eq!(
        title_in(&h, &id, "de").as_deref(),
        Some("Hallo Entwurf"),
        "the German draft goes live with the publish"
    );
    assert_eq!(
        slug_of(&h, &id).as_deref(),
        Some("drafted-slug"),
        "the draft's shared value goes live too"
    );
}

/// A non-default-locale publish still cannot CHANGE a shared field — the locale
/// lock judges the request's own fields — but the draft's shared values, saved
/// under the default locale, go live with it, and the other locales are left as
/// the draft recorded them.
#[test]
fn a_german_publish_makes_the_drafts_shared_values_live() {
    let h = setup();
    let id = seed(&h);
    let ctx = service_ctx(&h);

    let en = ctx_for(&h, "en");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("slug", "drafted-slug"), ("title", "Hello v2")]))
            .locale_ctx(Some(&en))
            .draft(true)
            .build(),
    )
    .expect("english draft");

    let de = ctx_for(&h, "de");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Hallo v2")]))
            .locale_ctx(Some(&de))
            .build(),
    )
    .expect("german publish");

    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo v2"));
    assert_eq!(
        title_in(&h, &id, "en").as_deref(),
        Some("Hello v2"),
        "a German publish takes the English title from the draft, not the row"
    );
    assert_eq!(
        slug_of(&h, &id).as_deref(),
        Some("drafted-slug"),
        "the draft's shared value goes live even from a German publish"
    );
}

/// Publishing without a pending draft changes nothing beyond the request: the
/// write-back only ever carries a draft that is actually pending.
#[test]
fn a_publish_without_a_pending_draft_writes_only_the_request() {
    let h = setup();
    let id = seed(&h);
    let ctx = service_ctx(&h);

    let en = ctx_for(&h, "en");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Hello v2")]))
            .locale_ctx(Some(&en))
            .build(),
    )
    .expect("publish");

    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Hello v2"));
    assert_eq!(title_in(&h, &id, "de").as_deref(), Some("Hallo"));
    assert_eq!(slug_of(&h, &id).as_deref(), Some("hello"));
}

/// A localized field's `access.update` rule is judged per locale on the
/// write-back: the snapshot carries one column per locale and publishing
/// writes every one, so a rule that denies `de` keeps the German column at
/// its stored value while the English one is published from the draft.
#[test]
fn the_write_back_judges_a_localized_field_per_locale() {
    let h = setup_with_locale_rule();
    let id = seed(&h);
    let ctx = service_ctx(&h);

    let en = ctx_for(&h, "en");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Draft EN")]))
            .locale_ctx(Some(&en))
            .draft(true)
            .build(),
    )
    .expect("english draft");

    let de = ctx_for(&h, "de");
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Draft DE")]))
            .locale_ctx(Some(&de))
            .draft(true)
            .build(),
    )
    .expect("german draft");

    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("slug", "published")]))
            .locale_ctx(Some(&en))
            .build(),
    )
    .expect("english publish");

    assert_eq!(title_in(&h, &id, "en").as_deref(), Some("Draft EN"));
    assert_eq!(
        title_in(&h, &id, "de").as_deref(),
        Some("Hallo"),
        "the rule denies German, so the German column keeps its stored value"
    );
}

/// The harness with a `title` rule that denies writes under the German locale.
fn setup_with_locale_rule() -> Harness {
    let h = setup();
    let access = h.tmp.path().join("access");
    std::fs::create_dir_all(&access).expect("access dir");
    std::fs::write(
        access.join("not_de.lua"),
        "return crap.any.access(function(context)\n\treturn context.locale ~= \"de\"\nend)\n",
    )
    .expect("rule");

    let mut def = make_def();
    def.fields[0] = FieldDefinition::builder("title", FieldType::Text)
        .localized(true)
        .access(FieldAccess {
            update: Some(HookRef::new("access.not_de")),
            ..Default::default()
        })
        .build();

    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.locale = h.locale.clone();
    let runner = HookRunner::builder()
        .config_dir(h.tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("runner");

    Harness { runner, def, ..h }
}
