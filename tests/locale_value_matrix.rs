//! Round-trip matrix for localized values. A draft saved with the published
//! values reads as the published document in every locale mode, and restoring a
//! version writes every locale's values back — for each shape a value takes
//! between a document and its columns: scalars, lists, timezone dates and JSON,
//! localized or shared, at the top level or inside a group.

use std::sync::Arc;

use crap_cms::{
    config::{CrapConfig, LocaleConfig},
    core::{
        DocumentFields, Registry,
        collection::{CollectionDefinition, VersionsConfig},
        field::{FieldDefinition, FieldType},
    },
    db::{DbPool, LocaleContext, LocaleMode, migrate, pool, query},
    hooks::lifecycle::HookRunner,
    service::{
        FindByIdInput, RunnerReadHooks, ServiceContext, WriteInput, create_document,
        find_document_by_id, restore_collection_version, update_document,
    },
};
use serde_json::{Map, Value, json};

const SLUG: &str = "matrix";

/// A leaf the German writes leave out, so a fallback read has work to do.
const UNTRANSLATED: &str = "text";

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
    locale: LocaleConfig,
}

/// One leaf of every value shape, named with `prefix`.
fn leaves(prefix: &str, localized: bool) -> Vec<FieldDefinition> {
    let leaf = |name: &str, field_type| {
        FieldDefinition::builder(&format!("{prefix}{name}"), field_type).localized(localized)
    };

    vec![
        leaf("text", FieldType::Text).build(),
        leaf("email", FieldType::Email).build(),
        leaf("number", FieldType::Number).build(),
        leaf("checkbox", FieldType::Checkbox).build(),
        leaf("tags", FieldType::Text).has_many(true).build(),
        leaf("scores", FieldType::Number).has_many(true).build(),
        leaf("starts", FieldType::Date).timezone(true).build(),
        leaf("meta", FieldType::Json).build(),
    ]
}

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new(SLUG);
    def.timestamps = true;

    let mut fields = leaves("l_", true);
    fields.extend(leaves("p_", false));
    fields.push(
        FieldDefinition::builder("g", FieldType::Group)
            .fields(leaves("", true))
            .build(),
    );
    fields.push(
        FieldDefinition::builder("lg", FieldType::Group)
            .localized(true)
            .fields(leaves("", false))
            .build(),
    );
    fields.push(
        FieldDefinition::builder("rows", FieldType::Array)
            .localized(true)
            .fields(vec![
                FieldDefinition::builder("caption", FieldType::Text).build(),
            ])
            .build(),
    );

    def.fields = fields;
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn setup(fallback: bool) -> Harness {
    let locale = LocaleConfig {
        default_locale: "en".to_string(),
        locales: vec!["en".to_string(), "de".to_string()],
        fallback,
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

/// Every locale mode a read can take — an unconfigured locale included.
fn modes() -> Vec<LocaleMode> {
    vec![
        LocaleMode::Default,
        LocaleMode::Single("en".to_string()),
        LocaleMode::Single("de".to_string()),
        LocaleMode::Single("fr".to_string()),
        LocaleMode::All,
    ]
}

fn locale_ctx(h: &Harness, mode: &LocaleMode) -> LocaleContext {
    LocaleContext {
        mode: mode.clone(),
        config: h.locale.clone(),
    }
}

/// Each leaf's value for `locale` in write number `n`.
fn leaf_values(locale: &str, n: i64) -> Vec<(&'static str, Value)> {
    let values = vec![
        ("text", json!(format!("{locale} text {n}"))),
        ("email", json!(format!("{locale}{n}@example.com"))),
        ("number", json!(n * 10 + 1)),
        ("checkbox", json!(n % 2 == 1)),
        ("tags", json!([format!("{locale}-a{n}"), format!("{locale}-b")])),
        ("scores", json!([n, n + 1])),
        ("starts", json!(format!("2026-0{n}-01T09:00"))),
        ("starts_tz", json!("Europe/Berlin")),
        ("meta", json!({ "locale": locale, "n": n })),
    ];

    values
        .into_iter()
        .filter(|(name, _)| locale == "en" || *name != UNTRANSLATED)
        .collect()
}

/// A write of every field for `locale`. Shared fields are included under every
/// locale; a non-default locale's write leaves them alone.
fn document(locale: &str, n: i64) -> DocumentFields {
    let mut data = DocumentFields::new();
    let mut group = Map::new();

    for (name, value) in leaf_values(locale, n) {
        data.insert(format!("l_{name}"), value.clone());
        data.insert(format!("p_{name}"), value.clone());
        group.insert(name.to_string(), value);
    }

    data.insert("g".to_string(), Value::Object(group.clone()));
    data.insert("lg".to_string(), Value::Object(group));
    data.insert(
        "rows".to_string(),
        json!([{ "caption": format!("{locale} row {n}") }]),
    );

    data
}

fn service_ctx(h: &Harness) -> ServiceContext<'_> {
    ServiceContext::collection(SLUG, &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build()
}

/// Create (without `id`) or update the document under `locale`, returning its id.
fn write(h: &Harness, id: Option<&str>, locale: &str, data: DocumentFields, draft: bool) -> String {
    let ctx = service_ctx(h);
    let locale_ctx = locale_ctx(h, &LocaleMode::Single(locale.to_string()));
    let input = WriteInput::builder(data)
        .locale_ctx(Some(&locale_ctx))
        .draft(draft)
        .build();

    let result = match id {
        Some(id) => update_document(&ctx, id, input),
        None => create_document(&ctx, input),
    };

    result.expect("write").0.id.to_string()
}

/// The document's fields read in `mode`, without the bookkeeping a draft and a
/// published read legitimately differ on (timestamps, status, row ids).
fn read(h: &Harness, id: &str, mode: &LocaleMode, draft: bool) -> DocumentFields {
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, None, None);
    let read_ctx = ServiceContext::collection(SLUG, &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .locale_config(Some(&h.locale))
        .build();
    let locale_ctx = locale_ctx(h, mode);

    let doc = find_document_by_id(
        &read_ctx,
        &FindByIdInput::builder(id)
            .locale_ctx(Some(&locale_ctx))
            .use_draft(draft)
            .build(),
    )
    .expect("read")
    .expect("document");

    let mut fields = doc.fields;
    for key in ["created_at", "updated_at", "_status"] {
        fields.remove(key);
    }
    strip_row_ids(fields.get_mut("rows"));

    fields
}

/// Drop `id` from array rows, at any locale nesting.
fn strip_row_ids(value: Option<&mut Value>) {
    match value {
        Some(Value::Array(rows)) => {
            for row in rows.iter_mut().filter_map(Value::as_object_mut) {
                row.remove("id");
            }
        }
        Some(Value::Object(by_locale)) => {
            for rows in by_locale.values_mut() {
                strip_row_ids(Some(rows));
            }
        }
        _ => {}
    }
}

fn restore_nth_newest(h: &Harness, id: &str, index: usize) {
    let conn = h.pool.get().unwrap();
    let versions = query::list_versions(&conn, SLUG, id, false, None, None).expect("versions");
    let version = versions.get(index).expect("that version").id.clone();
    drop(conn);

    restore_collection_version(&service_ctx(h), id, &version, &h.locale).expect("restore");
}

/// A draft saved with the published values reads as the published document, in
/// every locale mode, with fallback on and off.
#[test]
fn a_fresh_draft_reads_as_the_published_document() {
    for fallback in [true, false] {
        let h = setup(fallback);
        let id = write(&h, None, "en", document("en", 1), false);
        write(&h, Some(&id), "de", document("de", 1), false);

        let published: Vec<DocumentFields> =
            modes().iter().map(|m| read(&h, &id, m, false)).collect();

        write(&h, Some(&id), "de", document("de", 1), true);

        for (mode, expected) in modes().iter().zip(published) {
            assert_eq!(
                read(&h, &id, mode, true),
                expected,
                "draft read differs: fallback {fallback}, mode {mode:?}"
            );
        }
    }
}

/// Every locale's values read back as written, in an all-locales read — the
/// read the restore check compares.
#[test]
fn an_all_locales_read_returns_each_locales_values() {
    let h = setup(false);
    let id = write(&h, None, "en", document("en", 1), false);
    write(&h, Some(&id), "de", document("de", 1), false);

    let fields = read(&h, &id, &LocaleMode::All, false);

    assert_eq!(fields.get("l_tags"), Some(&json!({ "en": ["en-a1", "en-b"], "de": ["de-a1", "de-b"] })));
    assert_eq!(fields.get("l_scores"), Some(&json!({ "en": [1, 2], "de": [1, 2] })));
    assert_eq!(fields.get("l_number"), Some(&json!({ "en": 11, "de": 11 })));
}

/// Restoring a version writes every locale's values back as they were.
#[test]
fn restoring_a_version_reproduces_every_locale() {
    let h = setup(false);
    let id = write(&h, None, "en", document("en", 1), false);
    write(&h, Some(&id), "de", document("de", 1), false);
    let before = read(&h, &id, &LocaleMode::All, false);

    write(&h, Some(&id), "en", document("en", 2), false);
    write(&h, Some(&id), "de", document("de", 2), false);

    // Newest first: the German and English second writes, then the German first.
    restore_nth_newest(&h, &id, 2);

    assert_eq!(read(&h, &id, &LocaleMode::All, false), before);
}
