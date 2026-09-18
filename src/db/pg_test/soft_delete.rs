//! Postgres harness: the soft-delete transition drops UNIQUE constraints in
//! place and keeps every child row and foreign key.

#![cfg(all(test, feature = "postgres"))]

use super::support::*;
use super::{pg_test_pool, unique_slug};
use crate::db::{DbConnection, migrate::sync_all};

/// Postgres drops a collection's inline UNIQUE constraints in place, so a
/// `soft_delete` transition never rebuilds the table. The junction table
/// and `_versions_{slug}` therefore keep both their rows and the foreign
/// keys pointing at it — which a rebuild on this backend could not promise,
/// since the children's constraints follow the renamed table and the only
/// way to drop it takes them (or their rows) with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_the_soft_delete_transition_keeps_child_rows_and_their_foreign_keys() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("sdposts");
    let tags = unique_slug("sdtags");
    let junction = format!("{posts}_tags");
    let versions = format!("_versions_{posts}");

    sync_all(
        &pool,
        &soft_delete_registry(&posts, &tags, false),
        &no_locale(),
    )
    .expect("initial sync");

    {
        let conn = pool.get().expect("conn");
        conn.execute(&format!("INSERT INTO \"{tags}\" (id) VALUES ('t1')"), &[])
            .unwrap();
        conn.execute(
            &format!("INSERT INTO \"{posts}\" (id, title) VALUES ('p1', 'hello')"),
            &[],
        )
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO \"{junction}\" (parent_id, related_id, _order) \
                 VALUES ('p1', 't1', 0)"
            ),
            &[],
        )
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO \"{versions}\" (id, _parent, _version, _status, snapshot) \
                 VALUES ('v1', 'p1', 1, 'published', '{{}}')"
            ),
            &[],
        )
        .unwrap();
    }

    sync_all(
        &pool,
        &soft_delete_registry(&posts, &tags, true),
        &no_locale(),
    )
    .expect("soft-delete transition");

    let conn = pool.get().expect("conn");

    assert_eq!(row_count(&conn, &junction), 1, "the junction row survives");
    assert_eq!(row_count(&conn, &versions), 1, "the version row survives");

    for child in [&junction, &versions] {
        assert_eq!(
            foreign_keys_to(&conn, child, &posts),
            1,
            "{child} must still carry its foreign key to '{posts}'"
        );
    }
    assert_eq!(
        unique_constraints(&conn, &posts),
        0,
        "the inline UNIQUE must be gone after the transition"
    );

    drop_tables_matching(&conn, &posts);
    drop_tables_matching(&conn, &tags);
}
