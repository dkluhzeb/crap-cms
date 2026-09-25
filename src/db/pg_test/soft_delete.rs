//! Postgres harness: the soft-delete transition drops UNIQUE constraints in
//! place and keeps every child row and foreign key.

#![cfg(all(test, feature = "postgres"))]

use serde_json::json;

use super::support::*;
use super::{pg_test_pool, unique_slug};
use crate::{
    core::{CollectionDefinition, DocumentFields, ValidationError},
    db::{
        DbConnection,
        migrate::sync_all,
        query::ref_count::{UnavailableReferences, after_create_from_data, anchor_to_fields},
    },
};

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

/// A NEW reference to a trashed document is refused on Postgres exactly as on
/// `SQLite` — the check reads the target under `FOR UPDATE`, so it cannot move
/// to the trash before the count lands — and the refusal is reported on the
/// field holding the reference.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_new_reference_to_a_trashed_document_is_a_field_error() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let posts = unique_slug("rtposts");
    let tags = unique_slug("rttags");

    let mut registry = soft_delete_registry(&posts, &tags, false);
    let mut tag_def = CollectionDefinition::new(tags.as_str());
    tag_def.soft_delete = true;
    registry.register_collection(tag_def);

    sync_all(&pool, &registry, &no_locale()).expect("sync");

    let conn = pool.get().expect("conn");
    conn.execute(
        &format!(
            "INSERT INTO \"{tags}\" (id, _deleted_at) VALUES ('t1', '2026-01-01T00:00:00.000Z')"
        ),
        &[],
    )
    .unwrap();

    let fields = registry.get_collection(&posts).unwrap().fields.clone();
    let data: DocumentFields = [("tags".to_string(), json!(["t1"]))].into_iter().collect();

    let err = after_create_from_data(&conn, &fields, &data, &no_locale())
        .expect_err("a trashed target is refused");
    assert!(
        err.downcast_ref::<UnavailableReferences>().is_some(),
        "{err:#}"
    );

    let anchored = anchor_to_fields(err, &fields, &data);
    let ve = anchored
        .downcast_ref::<ValidationError>()
        .expect("reported on the field");
    assert_eq!(ve.errors[0].field, "tags");
}
