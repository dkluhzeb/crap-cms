//! Field-level `access.update` rules see the STORED document as
//! `ctx.document`, as documented — not the incoming patch. A rule that gates a
//! field on the document's owner must not be satisfiable by a caller who puts
//! `owner` in the very write the rule is judging.

use std::path::PathBuf;
use std::sync::Arc;

use crap_cms::config::{CrapConfig, LocaleConfig};
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldAccess, FieldDefinition, FieldType};
use crap_cms::core::{Document, DocumentFields, HookRef, Registry};
use crap_cms::db::{DbPool, migrate, pool, query};
use crap_cms::hooks::lifecycle::HookRunner;
use crap_cms::service::{
    OpDeadline, ServiceContext, UpdateManyOptions, WriteInput, update_document, update_many,
};
use serde_json::{Value, json};

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
}

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("payroll");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("owner", FieldType::Text).build(),
        FieldDefinition::builder("salary", FieldType::Number)
            .access(FieldAccess {
                update: Some(HookRef::new("access.owner_only")),
                ..Default::default()
            })
            .build(),
    ];
    def
}

fn setup() -> Harness {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/field_write_owner");
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let pool = pool::create_pool(tmp.path(), &config).expect("pool");
    let def = make_def();
    let shared = Registry::shared();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&pool, &registry, &config.locale).expect("sync");

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
    }
}

fn user(id: &str) -> Document {
    Document::new(id.to_string())
}

fn fields(pairs: &[(&str, Value)]) -> DocumentFields {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

fn seed(h: &Harness, owner: &str, salary: i64) -> String {
    let conn = h.pool.get().unwrap();
    let data = fields(&[("owner", json!(owner)), ("salary", json!(salary))]);

    query::create(&conn, "payroll", &h.def, &data, None)
        .expect("seed")
        .id
        .to_string()
}

fn salary(h: &Harness, id: &str) -> Option<f64> {
    let conn = h.pool.get().unwrap();

    query::find_by_id(&conn, "payroll", &h.def, id, None)
        .unwrap()
        .and_then(|d| d.fields.get("salary").and_then(Value::as_f64))
}

fn update_as(h: &Harness, who: &Document, id: &str, pairs: &[(&str, Value)]) {
    let ctx = ServiceContext::collection("payroll", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .user(Some(who))
        .build();

    update_document(&ctx, id, WriteInput::builder(fields(pairs)).build()).expect("update");
}

/// The founding bug: the rule read `owner` from the patch, so user B wrote
/// `owner = B` alongside the salary and passed the owner check on A's row.
#[test]
fn a_caller_cannot_pass_an_owner_rule_by_rewriting_owner_in_the_same_write() {
    let h = setup();
    let id = seed(&h, "user-a", 100);

    update_as(
        &h,
        &user("user-b"),
        &id,
        &[("owner", json!("user-b")), ("salary", json!(999))],
    );

    assert_eq!(
        salary(&h, &id),
        Some(100.0),
        "the rule must judge the stored owner, not the one the caller just wrote"
    );
}

/// Positive control: the real owner still passes the same rule. Also guards
/// the fixture's assumption about what the Lua context carries.
#[test]
fn the_real_owner_may_still_write_the_field() {
    let h = setup();
    let id = seed(&h, "user-a", 100);

    update_as(&h, &user("user-a"), &id, &[("salary", json!(150))]);

    assert_eq!(salary(&h, &id), Some(150.0));
}

/// The bulk path runs the same strip per row, so it must judge the same
/// stored owner.
#[test]
fn a_bulk_update_cannot_pass_an_owner_rule_by_rewriting_owner() {
    let h = setup();
    let id = seed(&h, "user-a", 100);
    let intruder = user("user-b");

    let ctx = ServiceContext::collection("payroll", &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .user(Some(&intruder))
        .build();

    update_many(
        &ctx,
        &[],
        &fields(&[("owner", json!("user-b")), ("salary", json!(999))]),
        &LocaleConfig::default(),
        &UpdateManyOptions {
            locale_ctx: None,
            run_hooks: true,
            draft: false,
            ui_locale: None,
            max_documents: 0,
            deadline: OpDeadline::none(),
        },
    )
    .expect("bulk update");

    assert_eq!(salary(&h, &id), Some(100.0));
}
