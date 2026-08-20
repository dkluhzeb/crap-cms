//! A user field named a SQL reserved word (`order`, `select`, `group`, …) is
//! allowed by field-name validation, so its column must be quoted at every
//! DDL/DML site or the collection fails to create/insert on Postgres (and some
//! keywords on `SQLite`). Regression net for the identifier-quoting chokepoint.

use crap_cms::config::CrapConfig;
use crap_cms::core::DocumentFields;
use crap_cms::core::Registry;
use crap_cms::core::collection::CollectionDefinition;
use crap_cms::core::field::{FieldDefinition, FieldType};
use crap_cms::db::{migrate, pool, query};
use serde_json::json;

fn reserved_word_collection() -> CollectionDefinition {
    let mut def = CollectionDefinition::new("items");
    def.timestamps = true;
    def.fields = vec![
        // All SQL reserved words, all valid field names.
        FieldDefinition::builder("order", FieldType::Number).build(),
        FieldDefinition::builder("select", FieldType::Text).build(),
        FieldDefinition::builder("group", FieldType::Text).build(),
    ];
    def
}

#[test]
fn reserved_word_field_names_migrate_write_and_read() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let db_pool = pool::create_pool(tmp.path(), &CrapConfig::default()).expect("pool");

    let shared = Registry::shared();
    let def = reserved_word_collection();
    shared.write().unwrap().register_collection(def.clone());
    let registry = Registry::snapshot(&shared);

    // Migration must not choke on the reserved-word columns (incl. FTS sync).
    migrate::sync_all(&db_pool, &registry, &CrapConfig::default().locale).expect("sync");

    // Write.
    let mut conn = db_pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut data = DocumentFields::new();
    data.insert("order".to_string(), json!(7));
    data.insert("select".to_string(), json!("hello"));
    data.insert("group".to_string(), json!("g1"));
    let doc = query::create(&tx, "items", &def, &data, None).expect("create");
    tx.commit().expect("commit");

    // Read back.
    let conn = db_pool.get().expect("conn");
    let found = query::find_by_id(&conn, "items", &def, &doc.id, None)
        .expect("find")
        .expect("doc exists");

    assert_eq!(found.fields.get("order"), Some(&json!(7)));
    assert_eq!(found.fields.get("select"), Some(&json!("hello")));
    assert_eq!(found.fields.get("group"), Some(&json!("g1")));

    // Update a reserved-word column too.
    let mut conn = db_pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let mut upd = DocumentFields::new();
    upd.insert("select".to_string(), json!("world"));
    query::update(&tx, "items", &def, &doc.id, &upd, None).expect("update");
    tx.commit().expect("commit");

    let conn = db_pool.get().expect("conn");
    let found = query::find_by_id(&conn, "items", &def, &doc.id, None)
        .expect("find")
        .expect("doc exists");
    assert_eq!(found.fields.get("select"), Some(&json!("world")));
    // (The pool disables SQLite's double-quoted-string misfeature so a quoted
    // *missing* column still errors — the localized-read footgun stays intact,
    // exercised by `db_locale`'s None-locale test.)
}
