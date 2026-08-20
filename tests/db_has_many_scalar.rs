//! Scalar has-many (`Text` / `Number` / `Select` / `Radio` with `has_many`)
//! stored as a JSON array in a `TEXT` column: the write edge canonicalizes
//! elements to the field type, and the read path parses the column back into a
//! typed array — so the value round-trips identically regardless of ingress
//! shape (API-typed vs admin-stringified). Regression net for the
//! Postgres-broken numeric column + the cross-surface shape divergence.

use crap_cms::config::CrapConfig;
use crap_cms::core::DocumentFields;
use crap_cms::core::Registry;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::db::{migrate, pool, query};
use serde_json::json;

fn scalar_list_collection() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("lists");
    def.timestamps = true;
    def.fields = vec![
        FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build(),
        FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build(),
    ];
    def
}

fn setup() -> (
    tempfile::TempDir,
    crap_cms::db::DbPool,
    CollectionDefinition,
) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let db_pool = pool::create_pool(tmp.path(), &CrapConfig::default()).expect("pool");

    let shared = Registry::shared();
    let def = scalar_list_collection();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);
    migrate::sync_all(&db_pool, &registry, &CrapConfig::default().locale).expect("sync");

    (tmp, db_pool, def)
}

fn write_and_read(
    def: &CollectionDefinition,
    pool: &crap_cms::db::DbPool,
    data: &DocumentFields,
) -> serde_json::Value {
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let doc = query::create(&tx, "lists", def, data, None).expect("create");
    tx.commit().expect("commit");

    let conn = pool.get().expect("conn");
    let found = query::find_by_id(&conn, "lists", def, &doc.id, None)
        .expect("find")
        .expect("doc exists");
    serde_json::to_value(found.fields.as_map()).expect("serialize")
}

#[test]
fn number_has_many_typed_input_round_trips_as_numbers() {
    let (_tmp, pool, def) = setup();
    let mut data = DocumentFields::new();
    data.insert("scores".to_string(), json!([1, 2, 3]));

    let read = write_and_read(&def, &pool, &data);
    assert_eq!(read["scores"], json!([1, 2, 3]));
}

#[test]
fn number_has_many_admin_stringified_input_round_trips_as_numbers() {
    let (_tmp, pool, def) = setup();

    // The admin form pre-normalizes a multi-value field into a JSON array of
    // strings; the read must still come back as numbers, matching the API path.
    let mut data = DocumentFields::new();
    data.insert("scores".to_string(), json!(r#"["1","2","3"]"#));

    let read = write_and_read(&def, &pool, &data);
    assert_eq!(read["scores"], json!([1, 2, 3]));
}

#[test]
fn text_has_many_round_trips_as_string_array() {
    let (_tmp, pool, def) = setup();
    let mut data = DocumentFields::new();
    data.insert("tags".to_string(), json!(["red", "blue"]));

    let read = write_and_read(&def, &pool, &data);
    assert_eq!(read["tags"], json!(["red", "blue"]));
}

#[test]
fn number_has_many_empty_list_round_trips_as_empty_array() {
    let (_tmp, pool, def) = setup();
    let mut data = DocumentFields::new();
    data.insert("scores".to_string(), json!([]));

    let read = write_and_read(&def, &pool, &data);
    assert_eq!(read["scores"], json!([]));
}
