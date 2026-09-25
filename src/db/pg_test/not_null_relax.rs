//! Postgres harness: the schema sync drops the `NOT NULL` older releases put
//! on required user-field columns, in place.

#![cfg(all(test, feature = "postgres"))]

use super::support::*;
use super::{pg_test_pool, unique_slug};
use crate::{
    core::{CollectionDefinition, FieldDefinition, FieldType, Registry},
    db::{DbConnection, migrate::sync_all},
};

/// Regression: a table created while `title` was required kept `NOT NULL` on
/// its column after `required` was removed, so every write omitting the value
/// failed. A removed field's column (`subtitle`) blocked every create the
/// same way. The sync relaxes both in place and keeps the rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_the_sync_relaxes_not_null_of_an_older_table() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("nnposts");

    {
        let conn = pool.get().expect("conn");
        conn.execute_ddl(
            &format!(
                "CREATE TABLE \"{posts}\" (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
                 subtitle TEXT NOT NULL, _ref_count INTEGER NOT NULL DEFAULT 0, \
                 created_at TEXT, updated_at TEXT)"
            ),
            &[],
        )
        .unwrap();
        conn.execute(
            &format!("INSERT INTO \"{posts}\" (id, title, subtitle) VALUES ('p1', 'a', 'b')"),
            &[],
        )
        .unwrap();
    }

    let mut def = CollectionDefinition::new(posts.as_str());
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
    let mut registry = Registry::new();
    registry.register_collection(def);

    sync_all(&pool, &registry, &no_locale()).expect("sync relaxes the old table");

    let conn = pool.get().expect("conn");

    conn.execute(&format!("INSERT INTO \"{posts}\" (id) VALUES ('p2')"), &[])
        .expect("neither the relaxed field nor the orphan column may block a create");

    assert_eq!(row_count(&conn, &posts), 2, "the old row is kept");

    drop_tables_matching(&conn, &posts);
}
