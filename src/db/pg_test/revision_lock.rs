//! Postgres harness: the revision precondition is checked and bumped
//! atomically — of two writers that both read revision `n`, exactly one lands.

#![cfg(all(test, feature = "postgres"))]

use std::{thread, time::Duration};

use tokio::task::spawn_blocking;

use super::support::{drop_tables_matching, no_locale};
use super::{pg_test_pool_sized, unique_slug};
use crate::{
    core::{CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Registry},
    db::{
        DbPool,
        migrate::sync_all,
        query::{self, RevisionAdvance},
    },
};

/// A synced one-field collection and one document in it, at revision 0.
fn seeded(pool: &DbPool, slug: &str) -> String {
    let mut def = CollectionDefinition::new(slug);
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

    let mut registry = Registry::new();
    registry.register_collection(def.clone());
    sync_all(pool, &registry, &no_locale()).expect("sync");

    let conn = pool.get().expect("conn");

    query::create(&conn, slug, &def, &DocumentFields::new(), None)
        .expect("create")
        .id
        .to_string()
}

/// Advance `id`'s revision expecting `expected`, in a transaction of its own.
fn advance_in_own_tx(
    pool: &DbPool,
    slug: &str,
    id: &str,
    expected: Option<i64>,
) -> RevisionAdvance {
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let outcome = query::advance_revision(&tx, slug, id, expected).expect("advance");
    tx.commit().expect("commit");

    outcome
}

/// Two editors loaded revision 0. The first writer's transaction advances the
/// row and is still open when the second one's conditional UPDATE arrives: it
/// waits for the row, re-reads it once the first commits, finds revision 1 and
/// matches nothing — the second write is refused, never stacked on top.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_stale_revision_loses_to_the_concurrent_writer() {
    let Some(pool) = pg_test_pool_sized(4, None) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let slug = unique_slug("revlock");
    let id = seeded(&pool, &slug);

    let mut first_conn = pool.get().expect("conn");
    let first = first_conn.transaction().expect("tx");
    assert_eq!(
        query::advance_revision(&first, &slug, &id, Some(0)).expect("advance"),
        RevisionAdvance::Advanced
    );

    let second = {
        let (pool, slug, id) = (pool.clone(), slug.clone(), id.clone());
        spawn_blocking(move || advance_in_own_tx(&pool, &slug, &id, Some(0)))
    };

    // Give the second writer time to reach the row before the first commits.
    thread::sleep(Duration::from_millis(300));
    first.commit().expect("commit");

    let second = second.await.expect("second writer");

    let conn = pool.get().expect("conn");
    let revision = query::read_revision(&conn, &slug, &id).expect("read");
    drop_tables_matching(&conn, &slug);

    assert_eq!(second, RevisionAdvance::Stale(1));
    assert_eq!(revision, Some(1), "only the first write moved the revision");
}

/// Unconditional writers never lose a bump to each other: two concurrent
/// advances leave the revision two further on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_concurrent_unconditional_writes_each_advance_the_revision() {
    let Some(pool) = pg_test_pool_sized(4, None) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let slug = unique_slug("revbump");
    let id = seeded(&pool, &slug);

    let writers: Vec<_> = (0..2)
        .map(|_| {
            let (pool, slug, id) = (pool.clone(), slug.clone(), id.clone());
            spawn_blocking(move || advance_in_own_tx(&pool, &slug, &id, None))
        })
        .collect();

    for writer in writers {
        assert_eq!(writer.await.expect("writer"), RevisionAdvance::Advanced);
    }

    let conn = pool.get().expect("conn");
    let revision = query::read_revision(&conn, &slug, &id).expect("read");
    drop_tables_matching(&conn, &slug);

    assert_eq!(revision, Some(2));
}
