//! The shape a value reads back in is the same on every surface: a checkbox
//! is a boolean and a JSON field is its parsed value — in a column, in a
//! group's column, in an array row, in a published read, in a draft read and
//! in a snapshot written before the read form changed.

use std::sync::Arc;

use crap_cms::{
    config::CrapConfig,
    core::{
        DocumentFields, Registry,
        collection::{CollectionDefinition, VersionsConfig},
        field::{FieldDefinition, FieldType},
    },
    db::{DbPool, migrate, pool, query},
    hooks::lifecycle::HookRunner,
    service::{
        FindByIdInput, RunnerReadHooks, ServiceContext, WriteInput, create_document,
        find_document_by_id, update_document,
    },
};
use serde_json::{Value, json};

const SLUG: &str = "shapes";

struct Harness {
    _tmp: tempfile::TempDir,
    pool: DbPool,
    runner: HookRunner,
    def: CollectionDefinition,
}

fn make_def() -> CollectionDefinition {
    let mut def = CollectionDefinition::new(SLUG);
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("done", FieldType::Checkbox).build(),
        FieldDefinition::builder("meta", FieldType::Json).build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("index", FieldType::Checkbox).build(),
            ])
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("flag", FieldType::Checkbox).build(),
                FieldDefinition::builder("extra", FieldType::Json).build(),
            ])
            .build(),
    ];
    def.versions = Some(VersionsConfig::new(true, 0));
    def
}

fn setup() -> Harness {
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
    }
}

/// A write of every field, the checkboxes checked and the JSON values sent as
/// the admin form sends them: text.
fn document() -> DocumentFields {
    let mut data = DocumentFields::new();
    data.insert("done".to_string(), json!("on"));
    data.insert("meta".to_string(), json!("{\"n\": 1}"));
    data.insert("seo".to_string(), json!({ "index": true }));
    data.insert(
        "items".to_string(),
        json!([{ "flag": "1", "extra": "{\"k\": [1, 2]}" }]),
    );

    data
}

fn write(h: &Harness, id: Option<&str>, draft: bool) -> String {
    let ctx = ServiceContext::collection(SLUG, &h.def)
        .pool(&h.pool)
        .runner(&h.runner)
        .build();
    let input = WriteInput::builder(document()).draft(draft).build();

    let result = match id {
        Some(id) => update_document(&ctx, id, input),
        None => create_document(&ctx, input),
    };

    result.expect("write").0.id.to_string()
}

/// The document's fields through the service read, published or draft.
fn read(h: &Harness, id: &str, draft: bool) -> DocumentFields {
    let conn = h.pool.get().unwrap();
    let hooks = RunnerReadHooks::new(&h.runner, &conn, None, None);
    let ctx = ServiceContext::collection(SLUG, &h.def)
        .conn(&conn)
        .read_hooks(&hooks)
        .build();

    find_document_by_id(&ctx, &FindByIdInput::builder(id).use_draft(draft).build())
        .expect("read")
        .expect("document")
        .fields
}

/// Every checkbox reads as `true` and every JSON value parsed, at each depth.
fn assert_read_shape(fields: &DocumentFields, surface: &str) {
    assert_eq!(fields.get("done"), Some(&json!(true)), "{surface}: column");
    assert_eq!(
        fields.get("meta"),
        Some(&json!({ "n": 1 })),
        "{surface}: JSON column"
    );
    assert_eq!(
        fields.get("seo"),
        Some(&json!({ "index": true })),
        "{surface}: group column"
    );

    let row = fields.get("items").and_then(Value::as_array).unwrap()[0].clone();
    assert_eq!(row["flag"], json!(true), "{surface}: array row");
    assert_eq!(
        row["extra"],
        json!({ "k": [1, 2] }),
        "{surface}: JSON in row"
    );
}

/// Regression: a checkbox column read as `1` while a checkbox inside a row
/// read as `true`, and a JSON column read as text while a JSON value inside a
/// row read parsed. The query read, the service read and a draft read agree.
#[test]
fn checkbox_and_json_read_the_same_at_every_depth() {
    let h = setup();
    let id = write(&h, None, false);

    let conn = h.pool.get().unwrap();
    let found = query::find_by_id(&conn, SLUG, &h.def, &id, None)
        .expect("find")
        .expect("document");
    assert_read_shape(&found.fields, "query::find_by_id");
    drop(conn);

    assert_read_shape(&read(&h, &id, false), "published");

    write(&h, Some(&id), true);
    assert_read_shape(&read(&h, &id, true), "draft");
}

/// A snapshot written before checkboxes and JSON read decoded — a checkbox as
/// `1`, JSON as text — reads decoded too.
#[test]
fn a_snapshot_in_the_old_column_form_reads_decoded() {
    let h = setup();
    let id = write(&h, None, false);

    let conn = h.pool.get().unwrap();
    let snapshot = json!({
        "done": 1,
        "meta": "{\"n\": 1}",
        "seo": { "index": 1 },
        "items": [{ "id": "r1", "flag": 1, "extra": "{\"k\": [1, 2]}" }],
    });
    query::create_version(&conn, SLUG, &id, "draft", &snapshot).expect("version");
    drop(conn);

    assert_read_shape(&read(&h, &id, true), "old snapshot");
}
