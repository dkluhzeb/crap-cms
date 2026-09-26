//! Postgres harness: a scalar has-many list inside an array row is stored in a
//! `TEXT` column — the JSON list the row writer binds — on a fresh table and
//! on one an older release typed numeric.

#![cfg(all(test, feature = "postgres"))]

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{
    pg_test_pool,
    support::{drop_tables_matching, no_locale},
    unique_slug,
};
use crate::{
    core::{CollectionDefinition, FieldDefinition, FieldType, Registry},
    db::{
        DbConnection,
        migrate::sync_all,
        query::{find_array_rows, set_array_rows},
    },
};

/// The `items` sub-fields: a number (a list when `has_many`), a text list and
/// a select list.
fn item_fields(has_many_scores: bool) -> Vec<FieldDefinition> {
    let list =
        |name: &str, ft: FieldType| FieldDefinition::builder(name, ft).has_many(true).build();

    vec![
        FieldDefinition::builder("scores", FieldType::Number)
            .has_many(has_many_scores)
            .build(),
        list("tags", FieldType::Text),
        list("kinds", FieldType::Select),
    ]
}

fn registry(posts: &str, has_many_scores: bool) -> Registry {
    let mut def = CollectionDefinition::new(posts);
    def.fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(item_fields(has_many_scores))
            .build(),
    ];

    let mut registry = Registry::new();
    registry.register_collection(def);

    registry
}

fn row(values: Value) -> HashMap<String, Value> {
    let Value::Object(map) = values else {
        panic!("a row is an object");
    };

    map.into_iter().collect()
}

fn write(conn: &dyn DbConnection, posts: &str, values: Value) {
    set_array_rows(
        conn,
        posts,
        "items",
        "p1",
        &[row(values)],
        &item_fields(true),
        None,
    )
    .expect("an array row holding scalar lists must save");
}

fn read(conn: &dyn DbConnection, posts: &str) -> Vec<Value> {
    find_array_rows(conn, posts, "items", "p1", &item_fields(true), None).unwrap()
}

/// Regression: the array table typed a has-many number sub-field's column
/// `DOUBLE PRECISION`, and the writer binds the list as JSON text — every
/// write of such a row failed. A row's lists survive a create, an update and
/// a read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_scalar_lists_in_array_rows_round_trip() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("alposts");
    sync_all(&pool, &registry(&posts, true), &no_locale()).expect("sync");

    let conn = pool.get().expect("conn");
    conn.execute(&format!("INSERT INTO \"{posts}\" (id) VALUES ('p1')"), &[])
        .unwrap();

    write(
        &conn,
        &posts,
        json!({ "scores": [1, 2.5], "tags": ["a", "b"], "kinds": ["x"] }),
    );

    let found = read(&conn, &posts);
    assert_eq!(found[0]["scores"], json!([1, 2.5]));
    assert_eq!(found[0]["tags"], json!(["a", "b"]));
    assert_eq!(found[0]["kinds"], json!(["x"]));

    let id = found[0]["id"].clone();
    write(
        &conn,
        &posts,
        json!({ "id": id, "scores": [3], "tags": [], "kinds": ["y", "z"] }),
    );

    let found = read(&conn, &posts);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["scores"], json!([3]));
    assert_eq!(found[0]["kinds"], json!(["y", "z"]));

    drop_tables_matching(&conn, &posts);
}

/// A number sub-field switched to `has_many` over a numeric column: the sync
/// retypes the column to `TEXT`, the stored single value becomes a one-element
/// list, and the row saves again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_numeric_array_column_is_reconciled_to_a_list() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("alrposts");
    sync_all(&pool, &registry(&posts, false), &no_locale()).expect("single-number sync");

    {
        let conn = pool.get().expect("conn");
        conn.execute(&format!("INSERT INTO \"{posts}\" (id) VALUES ('p1')"), &[])
            .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO \"{posts}_items\" (id, parent_id, _order, scores) \
                 VALUES ('r1', 'p1', 0, 5)"
            ),
            &[],
        )
        .unwrap();
    }

    sync_all(&pool, &registry(&posts, true), &no_locale()).expect("has-many sync");

    let conn = pool.get().expect("conn");
    assert_eq!(read(&conn, &posts)[0]["scores"], json!([5]));

    write(&conn, &posts, json!({ "id": "r1", "scores": [5, 6] }));
    assert_eq!(read(&conn, &posts)[0]["scores"], json!([5, 6]));

    drop_tables_matching(&conn, &posts);
}
