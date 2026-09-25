//! Postgres harness: turning `has_many` on or off for a relationship carries
//! its values between the column and the junction table.

#![cfg(all(test, feature = "postgres"))]

use super::support::*;
use super::{pg_test_pool, unique_slug};
use serde_json::{Value, from_str, json};

use crate::{
    core::{
        BlockDefinition, CollectionDefinition, FieldDefinition, FieldType, Registry,
        RelationshipConfig,
    },
    db::{DbConnection, migrate::sync_all},
};

/// `users` and `posts` whose `author` relationship has the given cardinality.
fn registry(posts: &str, users: &str, has_many: bool) -> Registry {
    let mut def = CollectionDefinition::new(posts);
    def.fields = vec![
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new(users, has_many))
            .build(),
    ];

    let mut registry = Registry::new();
    registry.register_collection(CollectionDefinition::new(users));
    registry.register_collection(def);

    registry
}

fn text(conn: &dyn DbConnection, sql: &str) -> Option<String> {
    conn.query_one(sql, &[])
        .unwrap()
        .and_then(|row| row.opt_text_at(0))
}

/// Regression: toggling `has_many` stranded every stored value — the column's
/// in a column nothing read, the junction's in a table nothing read. Both
/// directions carry them, and the target's reference count follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_toggling_has_many_carries_the_values_both_ways() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("hmposts");
    let users = unique_slug("hmusers");
    let junction = format!("{posts}_author");

    sync_all(&pool, &registry(&posts, &users, false), &no_locale()).expect("has-one sync");

    {
        let conn = pool.get().expect("conn");
        conn.execute(&format!("INSERT INTO \"{users}\" (id) VALUES ('u1')"), &[])
            .unwrap();
        conn.execute(
            &format!("INSERT INTO \"{posts}\" (id, author) VALUES ('p1', 'u1')"),
            &[],
        )
        .unwrap();
    }

    sync_all(&pool, &registry(&posts, &users, true), &no_locale()).expect("has-many sync");

    {
        let conn = pool.get().expect("conn");
        assert_eq!(
            text(
                &conn,
                &format!("SELECT related_id FROM \"{junction}\" WHERE parent_id = 'p1'")
            ),
            Some("u1".to_string())
        );
        assert_eq!(
            text(
                &conn,
                &format!("SELECT _ref_count::text FROM \"{users}\" WHERE id = 'u1'")
            ),
            Some("1".to_string())
        );

        conn.execute(&format!("UPDATE \"{posts}\" SET author = NULL"), &[])
            .unwrap();
    }

    sync_all(&pool, &registry(&posts, &users, false), &no_locale()).expect("back to has-one");

    let conn = pool.get().expect("conn");
    assert_eq!(
        text(
            &conn,
            &format!("SELECT author FROM \"{posts}\" WHERE id = 'p1'")
        ),
        Some("u1".to_string())
    );

    drop_tables_matching(&conn, &posts);
    drop_tables_matching(&conn, &users);
}

/// `users` and `posts` holding `author` (of the given cardinality) inside an
/// `items` array row and a `hero` block.
fn row_registry(posts: &str, users: &str, has_many: bool) -> Registry {
    let author = || {
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new(users, has_many))
            .build()
    };

    let mut def = CollectionDefinition::new(posts);
    def.fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![author()])
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new("hero", vec![author()])])
            .build(),
    ];

    let mut registry = Registry::new();
    registry.register_collection(CollectionDefinition::new(users));
    registry.register_collection(def);

    registry
}

/// Regression: turning `has_many` off for a reference inside an array or
/// blocks row left the rows' one-element lists behind; the recount then saw
/// no reference and the target became deletable. The rows hold the single id
/// again and the count follows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_turning_has_many_off_inside_rows_keeps_the_single_values() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("hmrposts");
    let users = unique_slug("hmrusers");

    sync_all(&pool, &row_registry(&posts, &users, true), &no_locale()).expect("has-many sync");

    {
        let conn = pool.get().expect("conn");
        conn.execute(
            &format!("INSERT INTO \"{users}\" (id) VALUES ('u1'), ('u2')"),
            &[],
        )
        .unwrap();
        conn.execute(&format!("INSERT INTO \"{posts}\" (id) VALUES ('p1')"), &[])
            .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO \"{posts}_items\" (id, parent_id, _order, author) \
                 VALUES ('i1', 'p1', 0, '[\"u1\"]')"
            ),
            &[],
        )
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO \"{posts}_content\" (id, parent_id, _order, _block_type, data) \
                 VALUES ('b1', 'p1', 0, 'hero', '{{\"author\":[\"u2\"]}}')"
            ),
            &[],
        )
        .unwrap();
    }

    sync_all(&pool, &row_registry(&posts, &users, false), &no_locale()).expect("has-one sync");

    let conn = pool.get().expect("conn");
    assert_eq!(
        text(&conn, &format!("SELECT author FROM \"{posts}_items\"")),
        Some("u1".to_string())
    );

    let data: Value = from_str(
        &text(
            &conn,
            &format!("SELECT data::text FROM \"{posts}_content\""),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(data, json!({ "author": "u2" }));

    for user in ["u1", "u2"] {
        assert_eq!(
            text(
                &conn,
                &format!("SELECT _ref_count::text FROM \"{users}\" WHERE id = '{user}'")
            ),
            Some("1".to_string()),
            "{user} keeps its count"
        );
    }

    drop_tables_matching(&conn, &posts);
    drop_tables_matching(&conn, &users);
}
