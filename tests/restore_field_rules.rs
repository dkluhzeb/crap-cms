//! Version restore honors field-level `access.update` rules the way an update
//! does: a write-denied field keeps its live value in every locale, and the
//! rule judges the live document — not the snapshot being restored. It honors
//! `access.read` the same way: a write never changes a value its writer cannot
//! read, so a field the restorer cannot read keeps its current value.

use std::path::PathBuf;
use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::{CollectionDefinition, VersionsConfig};
use crap_cms::core::field::{FieldAccess, FieldDefinition, FieldType};
use crap_cms::core::{Document, DocumentFields, HookRef, Registry};
use crap_cms::db::{DbPool, LocaleContext, LocaleMode, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{ServiceContext, WriteInput, restore_collection_version, update_document};
use serde_json::json;

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
    locale: LocaleConfig,
}

/// `salary` is localized and writable only by the document's owner; `memo` is
/// localized and readable only by the document's owner.
fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("payroll");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("owner", FieldType::Text).build(),
        FieldDefinition::builder("title", FieldType::Text)
            .localized(true)
            .build(),
        FieldDefinition::builder("salary", FieldType::Text)
            .localized(true)
            .access(FieldAccess {
                update: Some(HookRef::new("access.owner_only")),
                ..Default::default()
            })
            .build(),
        FieldDefinition::builder("memo", FieldType::Text)
            .localized(true)
            .access(FieldAccess {
                read: Some(HookRef::new("access.owner_only")),
                ..Default::default()
            })
            .build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn setup() -> Harness {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/field_write_owner");
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
        .config_dir(&fixture)
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

/// Seed a document owned by `owner` with a salary and a memo in both locales.
fn seed(h: &Harness, owner: &str) -> String {
    let conn = h.pool.get().unwrap();
    let en = locale_ctx(h, "en");
    let doc = query::create(
        &conn,
        "payroll",
        &h.def,
        &fields(&[
            ("owner", owner),
            ("title", "T1"),
            ("salary", "100"),
            ("memo", "m1"),
        ]),
        Some(&en),
    )
    .expect("create");

    let de = locale_ctx(h, "de");
    query::update(
        &conn,
        "payroll",
        &h.def,
        &doc.id,
        &fields(&[("salary", "90"), ("memo", "m1-de")]),
        Some(&de),
    )
    .expect("german salary");

    doc.id.to_string()
}

/// A published update through the service, which records a version.
fn update_as(h: &Harness, who: Option<&Document>, id: &str, pairs: &[(&str, &str)]) {
    let en = locale_ctx(h, "en");
    let ctx = ServiceContext::collection("payroll", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .user(who)
        .override_access(who.is_none())
        .build();

    update_document(
        &ctx,
        id,
        WriteInput::builder(fields(pairs))
            .locale_ctx(Some(&en))
            .build(),
    )
    .expect("update");
}

/// Restore the OLDEST version of `id` as `who`.
fn restore_oldest_as(h: &Harness, who: &Document, id: &str) {
    let conn = h.pool.get().unwrap();
    let versions = query::list_versions(&conn, "payroll", id, false, None, None).expect("versions");
    let oldest = versions.last().expect("a version").id.clone();
    drop(conn);

    let ctx = ServiceContext::collection("payroll", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .locale_config(Some(&h.locale))
        .user(Some(who))
        .build();

    restore_collection_version(&ctx, id, &oldest, &h.locale).expect("restore");
}

fn value_in(h: &Harness, id: &str, field: &str, locale: &str) -> Option<String> {
    let conn = h.pool.get().unwrap();

    query::find_by_id(&conn, "payroll", &h.def, id, Some(&locale_ctx(h, locale)))
        .unwrap()
        .and_then(|d| d.get_str(field).map(str::to_string))
}

/// A restore by a caller denied on `salary` restores the other fields and
/// leaves `salary` exactly as it is live — in every locale. Dropping the
/// field from the snapshot used to make restore write NULL into each locale.
#[test]
fn restore_leaves_a_denied_localized_field_untouched() {
    let h = setup();
    let owner = Document::new("user-a".to_string());
    let intruder = Document::new("user-b".to_string());
    let id = seed(&h, "user-a");

    update_as(&h, Some(&owner), &id, &[("title", "T2")]);
    update_as(&h, Some(&owner), &id, &[("title", "T3"), ("salary", "150")]);

    restore_oldest_as(&h, &intruder, &id);

    assert_eq!(value_in(&h, &id, "title", "en").as_deref(), Some("T2"));
    assert_eq!(value_in(&h, &id, "salary", "en").as_deref(), Some("150"));
    assert_eq!(value_in(&h, &id, "salary", "de").as_deref(), Some("90"));
}

/// Ownership moved from B to A after the snapshot was taken. B restoring that
/// snapshot must not pass the owner rule on the strength of the old `owner`
/// value inside it.
#[test]
fn restore_judges_owner_rules_against_the_live_document() {
    let h = setup();
    let former_owner = Document::new("user-b".to_string());
    let id = seed(&h, "user-b");

    update_as(&h, Some(&former_owner), &id, &[("title", "T2")]);
    update_as(&h, None, &id, &[("owner", "user-a"), ("salary", "200")]);

    restore_oldest_as(&h, &former_owner, &id);

    assert_eq!(
        value_in(&h, &id, "salary", "en").as_deref(),
        Some("200"),
        "the rule must judge the live owner (user-a), not the snapshot's"
    );
}

/// The owner edits the memo after the oldest version was recorded, then
/// restores it: the memo goes back with the rest of the document.
#[test]
fn restore_rolls_back_a_field_its_restorer_can_read() {
    let h = setup();
    let owner = Document::new("user-a".to_string());
    let id = seed(&h, "user-a");

    update_as(&h, Some(&owner), &id, &[("title", "T2")]);
    update_as(&h, Some(&owner), &id, &[("title", "T3"), ("memo", "m2")]);

    restore_oldest_as(&h, &owner, &id);

    assert_eq!(value_in(&h, &id, "title", "en").as_deref(), Some("T2"));
    assert_eq!(value_in(&h, &id, "memo", "en").as_deref(), Some("m1"));
}

/// Regression: a restore wrote the snapshot's value into every field, so a
/// restorer overwrote values they could not see. A field the restorer cannot
/// read keeps its current value in every locale — the restore is partial for
/// them — while every field they can read is restored.
#[test]
fn restore_leaves_a_field_its_restorer_cannot_read_at_its_current_value() {
    let h = setup();
    let owner = Document::new("user-a".to_string());
    let editor = Document::new("user-b".to_string());
    let id = seed(&h, "user-a");

    update_as(&h, Some(&owner), &id, &[("title", "T2")]);
    update_as(&h, Some(&owner), &id, &[("title", "T3"), ("memo", "m2")]);

    restore_oldest_as(&h, &editor, &id);

    assert_eq!(value_in(&h, &id, "title", "en").as_deref(), Some("T2"));
    assert_eq!(value_in(&h, &id, "memo", "en").as_deref(), Some("m2"));
    assert_eq!(value_in(&h, &id, "memo", "de").as_deref(), Some("m1-de"));
}
