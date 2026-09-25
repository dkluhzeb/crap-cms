//! Postgres harness: an unpublish snapshots the row its status write lands on,
//! not one a concurrent writer replaced in between.

#![cfg(all(test, feature = "postgres"))]

use std::{thread, time::Duration};

use serde_json::json;
use tokio::task::spawn_blocking;

use super::support::{drop_tables_matching, no_locale};
use super::{pg_test_pool_sized, unique_slug};
use crate::{
    core::{
        CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Registry, VersionsConfig,
    },
    db::{
        DbConnection, DbPool,
        migrate::sync_all,
        query::{self, find_latest_version},
    },
    service::{ServiceContext, persist_unpublish},
};

/// A drafts-enabled collection with one `title` field.
fn drafts_def(slug: &str) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.versions = Some(VersionsConfig::new(true, 0));
    def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

    def
}

fn title(value: &str) -> DocumentFields {
    let mut data = DocumentFields::new();
    data.insert("title".to_string(), json!(value));

    data
}

/// Unpublish `id` in its own transaction and commit.
fn unpublish_in_own_tx(pool: &DbPool, def: &CollectionDefinition, id: &str) {
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");

    let ctx = ServiceContext::collection(&def.slug, def).conn(&tx).build();
    persist_unpublish(&ctx, id).expect("unpublish");

    tx.commit().expect("commit");
}

/// Regression: the unpublish read the row before any lock and took its first
/// lock at the status UPDATE. A writer committing in between left the draft
/// snapshot — the pending draft the next publish adopts — holding the stale
/// pre-write content. Locking before the read makes the unpublish wait for the
/// writer and snapshot what it committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_unpublish_snapshots_the_row_a_concurrent_writer_committed() {
    let Some(pool) = pg_test_pool_sized(4, None) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let slug = unique_slug("unpublock");
    let def = drafts_def(&slug);

    let mut registry = Registry::new();
    registry.register_collection(def.clone());
    sync_all(&pool, &registry, &no_locale()).expect("sync");

    let id = {
        let conn = pool.get().expect("conn");
        query::create(&conn, &slug, &def, &title("A"), None)
            .expect("create")
            .id
            .to_string()
    };

    // The writer holds the row lock while it rewrites the title.
    let mut writer_conn = pool.get().expect("conn");
    let writer = writer_conn.transaction().expect("tx");
    writer.lock_row(&slug, &id).expect("lock");

    let unpublisher = {
        let (pool, def, id) = (pool.clone(), def.clone(), id.clone());
        spawn_blocking(move || unpublish_in_own_tx(&pool, &def, &id))
    };

    // Give the unpublish time to reach the row before the writer commits.
    thread::sleep(Duration::from_millis(300));

    query::update(&writer, &slug, &def, &id, &title("B"), None).expect("update");
    writer.commit().expect("commit");

    unpublisher.await.expect("unpublish task");

    let conn = pool.get().expect("conn");
    let snapshot = find_latest_version(&conn, &slug, &id)
        .expect("read version")
        .expect("the unpublish recorded a draft version")
        .snapshot;

    drop_tables_matching(&conn, &slug);

    assert_eq!(
        snapshot["title"],
        json!("B"),
        "the draft snapshot must hold what the concurrent writer committed"
    );
}
