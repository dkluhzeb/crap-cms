//! Full-text search on a localized, soft-delete collection through the
//! service write/read paths — the shapes the per-document index sync must
//! survive: locale-aliased re-reads, undelete, and the trash view.

use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::core::{DocumentFields, Registry};
use crap_cms::db::{
    DbConnection, DbPool, DbValue, FindQuery, LocaleContext, LocaleMode, migrate, pool,
};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    FindDocumentsInput, RunnerReadHooks, ServiceContext, WriteInput, create_document,
    delete_document, find_documents, undelete_document, update_document,
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
    def.soft_delete = true;
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build(),
        FieldDefinition::builder("body", FieldType::Textarea).build(),
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

fn locale_ctx(h: &Harness, locale: &str) -> LocaleContext {
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

/// Raw index-membership probe: which ids match `term` in the FTS table.
fn fts_ids(h: &Harness, term: &str) -> Vec<String> {
    let conn = h.pool.get().unwrap();
    conn.query_all(
        "SELECT id FROM _fts_pages WHERE _fts_pages MATCH ?1",
        &[DbValue::Text(format!("\"{term}\" *"))],
    )
    .unwrap()
    .iter()
    .map(|r| r.get_string("id").unwrap())
    .collect()
}

fn create(h: &Harness, locale: &str, data: DocumentFields) -> String {
    let lctx = locale_ctx(h, locale);
    let ctx = ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build();
    let (doc, _) = create_document(
        &ctx,
        WriteInput::builder(data).locale_ctx(Some(&lctx)).build(),
    )
    .expect("create");

    doc.id.to_string()
}

fn search(h: &Harness, term: &str, trash: bool) -> Vec<String> {
    let lctx = locale_ctx(h, "en");
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, None, None);
    let ctx = ServiceContext::collection("pages", &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .locale_config(Some(&h.locale))
        .build();
    let fq = FindQuery::builder().search(Some(term.to_string())).build();
    let input = FindDocumentsInput::builder(&fq)
        .locale_ctx(Some(&lctx))
        .trash(trash)
        .build();

    find_documents(&ctx, &input)
        .expect("find")
        .docs
        .into_iter()
        .map(|d| d.id.to_string())
        .collect()
}

/// A service create/update on a localized collection re-reads the row under a
/// locale alias (`title__en AS title`); the index sync must still see the
/// per-locale column text rather than indexing every column as empty.
#[test]
fn localized_create_and_update_are_indexed() {
    let h = setup();
    let id = create(
        &h,
        "en",
        fields(&[("title", "Aurora Handbook"), ("body", "Shared prose")]),
    );

    assert_eq!(
        fts_ids(&h, "Aurora"),
        vec![id.clone()],
        "title indexed on create"
    );
    assert_eq!(
        fts_ids(&h, "Shared"),
        vec![id.clone()],
        "body indexed on create"
    );

    // A German translation adds to the index without blanking the English text.
    let de = locale_ctx(&h, "de");
    let ctx = ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build();
    update_document(
        &ctx,
        &id,
        WriteInput::builder(fields(&[("title", "Nordlicht Handbuch")]))
            .locale_ctx(Some(&de))
            .build(),
    )
    .expect("update de");

    assert_eq!(
        fts_ids(&h, "Nordlicht"),
        vec![id.clone()],
        "de title indexed"
    );
    assert_eq!(fts_ids(&h, "Aurora"), vec![id.clone()], "en title survives");
    assert_eq!(fts_ids(&h, "Shared"), vec![id.clone()], "body survives");

    // The service search path agrees with the raw probe.
    assert_eq!(search(&h, "Aurora", false), vec![id]);
}

/// Undelete on a localized collection must read the restored row under the
/// default locale context, not the bare (non-existent) column names.
#[test]
fn undelete_works_on_a_localized_collection() {
    let h = setup();
    let id = create(&h, "en", fields(&[("title", "Lazarus"), ("body", "risen")]));

    let ctx = ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build();
    delete_document(&ctx, &id, None, Some(&h.locale)).expect("soft delete");

    let restored = undelete_document(&ctx, &id).expect("undelete on a localized collection");
    assert_eq!(restored.get_str("title"), Some("Lazarus"));

    assert_eq!(search(&h, "Lazarus", false), vec![id], "searchable again");
}

/// Soft-deleted rows stay in the index so the trash view can be searched;
/// the normal view still never returns them.
#[test]
fn trash_view_search_finds_soft_deleted_rows() {
    let h = setup();
    let id = create(&h, "en", fields(&[("title", "Unicorn"), ("body", "magic")]));

    let ctx = ServiceContext::collection("pages", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .build();
    delete_document(&ctx, &id, None, Some(&h.locale)).expect("soft delete");

    assert!(
        search(&h, "Unicorn", false).is_empty(),
        "hidden from the normal view"
    );
    assert_eq!(
        search(&h, "Unicorn", true),
        vec![id],
        "found in the trash view"
    );
}
