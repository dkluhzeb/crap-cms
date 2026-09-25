//! Postgres harness: the queries relationship population adds — the grouped
//! (per-parent limited) join lookup and the batched pending-draft lookup —
//! behave as on `SQLite`.

#![cfg(all(test, feature = "postgres"))]

use serde_json::json;

use super::{pg_test_pool, support::*, unique_slug};
use crate::{
    core::{CollectionDefinition, FieldDefinition, FieldType, Registry, RelationshipConfig},
    db::{
        DbConnection, Filter, FilterClause, FilterOp, FindQuery,
        migrate::sync_all,
        query::{
            GroupLimit, GroupedFind, create_version, find_grouped, find_latest_draft_versions,
        },
    },
};

/// `posts` with an `author` relationship to `users`.
fn posts_def(posts: &str, users: &str) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(posts);
    def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new(users, false))
            .build(),
    ];
    def
}

/// Each parent keeps its first rows in sort order, however many it has — the
/// window query a join's limit runs, on Postgres.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_grouped_find_keeps_the_first_rows_per_parent() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("grpposts");
    let users = unique_slug("grpusers");
    let def = posts_def(&posts, &users);

    let mut registry = Registry::new();
    registry.register_collection(CollectionDefinition::new(users.as_str()));
    registry.register_collection(def.clone());
    sync_all(&pool, &registry, &no_locale()).expect("sync");

    let conn = pool.get().expect("conn");
    conn.execute(
        &format!(
            "INSERT INTO \"{posts}\" (id, title, author) VALUES \
             ('p1', 'c', 'u1'), ('p2', 'a', 'u1'), ('p3', 'b', 'u1'), ('p4', 'z', 'u2')"
        ),
        &[],
    )
    .unwrap();

    let query = FindQuery::builder()
        .filters(vec![FilterClause::Single(Filter {
            field: "author".to_string(),
            op: FilterOp::In(vec!["u1".into(), "u2".into()]),
        })])
        .order_by(Some("title".to_string()))
        .build();
    let find = GroupedFind::builder(&posts, &def, &query, GroupLimit::new("author", 2)).build();

    let titles: Vec<(String, String)> = find_grouped(&conn, &find)
        .unwrap()
        .iter()
        .map(|d| {
            (
                d.get_str("author").unwrap_or_default().to_string(),
                d.get_str("title").unwrap_or_default().to_string(),
            )
        })
        .collect();

    let of = |author: &str| -> Vec<&str> {
        titles
            .iter()
            .filter(|(a, _)| a == author)
            .map(|(_, t)| t.as_str())
            .collect()
    };

    assert_eq!(of("u1"), vec!["a", "b"]);
    assert_eq!(of("u2"), vec!["z"]);

    drop_tables_matching(&conn, &posts);
    drop_tables_matching(&conn, &users);
}

/// Only a parent whose latest version is a draft has a pending draft.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_latest_draft_versions_are_found_per_parent() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let slug = unique_slug("pendingdrafts");
    let conn = pool.get().expect("conn");
    conn.execute_ddl(
        &format!(
            "CREATE TABLE \"_versions_{slug}\" (\
                id TEXT PRIMARY KEY, _parent TEXT NOT NULL, _version INTEGER NOT NULL, \
                _status TEXT NOT NULL, _latest INTEGER NOT NULL DEFAULT 0, \
                snapshot TEXT NOT NULL, created_at TEXT, updated_at TEXT)"
        ),
        &[],
    )
    .expect("versions table");

    create_version(&conn, &slug, "a", "published", &json!({ "t": "a1" })).unwrap();
    create_version(&conn, &slug, "a", "draft", &json!({ "t": "a2" })).unwrap();
    create_version(&conn, &slug, "b", "draft", &json!({ "t": "b1" })).unwrap();
    create_version(&conn, &slug, "b", "published", &json!({ "t": "b2" })).unwrap();

    let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let found = find_latest_draft_versions(&conn, &slug, &ids).unwrap();

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found["a"].snapshot, json!({ "t": "a2" }));

    drop_tables_matching(&conn, &format!("_versions_{slug}"));
}
