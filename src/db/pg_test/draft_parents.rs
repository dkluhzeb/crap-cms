//! Postgres harness: the drafted-file owner lookup reads a snapshot column the
//! way `SQLite` does.

#![cfg(all(test, feature = "postgres"))]

use serde_json::json;

use super::{pg_test_pool, support::drop_tables_matching, unique_slug};
use crate::db::{
    DbConnection,
    query::{create_version, find_draft_parents_naming},
};

/// A bare `_versions_{slug}` table — the lookup reads nothing else.
fn create_versions_table(conn: &dyn DbConnection, slug: &str) {
    conn.execute_ddl(
        &format!(
            "CREATE TABLE \"_versions_{slug}\" (\
                id TEXT PRIMARY KEY, \
                _parent TEXT NOT NULL, \
                _version INTEGER NOT NULL, \
                _status TEXT NOT NULL, \
                _latest INTEGER NOT NULL DEFAULT 0, \
                snapshot TEXT NOT NULL, \
                created_at TEXT, \
                updated_at TEXT\
            )"
        ),
        &[],
    )
    .expect("create versions table");
}

/// The `#>>` extraction over the TEXT snapshot matches a whole column value —
/// including one carrying a slash, a quote, a backslash and non-ASCII text,
/// which the JSON encoding escapes — only in the named columns and only on a
/// latest draft, exactly as `SQLite`'s `json_extract` does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_draft_parents_match_whole_column_values() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let slug = unique_slug("draftparents");
    let conn = pool.get().expect("conn");
    create_versions_table(&conn, &slug);

    let url = "/uploads/media/ab_c%1_ph\"o\\t\u{f6}-\u{65e5}.png";
    let columns = ["url".to_string(), "thumb_url".to_string()];

    create_version(&conn, &slug, "p1", "draft", &json!({ "url": url })).unwrap();
    create_version(&conn, &slug, "p2", "draft", &json!({ "thumb_url": url })).unwrap();
    create_version(&conn, &slug, "p3", "published", &json!({ "url": url })).unwrap();
    create_version(&conn, &slug, "p4", "draft", &json!({ "caption": url })).unwrap();
    create_version(
        &conn,
        &slug,
        "p5",
        "draft",
        &json!({ "url": format!("{url}.webp") }),
    )
    .unwrap();
    create_version(&conn, &slug, "p6", "draft", &json!({ "url": url })).unwrap();
    create_version(&conn, &slug, "p6", "draft", &json!({ "url": "/newer" })).unwrap();

    let mut found = find_draft_parents_naming(&conn, &slug, &columns, url).unwrap();
    found.sort();

    drop_tables_matching(&conn, &slug);

    assert_eq!(found, vec!["p1".to_string(), "p2".to_string()]);
}
