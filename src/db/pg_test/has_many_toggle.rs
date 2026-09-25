//! Postgres harness: turning `has_many` on or off for a relationship carries
//! its values between the column and the junction table.

#![cfg(all(test, feature = "postgres"))]

use super::support::*;
use super::{pg_test_pool, unique_slug};
use crate::{
    core::{CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig},
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
