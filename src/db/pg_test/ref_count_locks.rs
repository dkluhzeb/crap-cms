//! Postgres harness: concurrent writes that reference the same targets never
//! deadlock on the targets' reference-count row locks.

#![cfg(all(test, feature = "postgres"))]

use anyhow::Result;
use serde_json::json;
use tokio::task::spawn_blocking;

use super::support::{drop_tables_matching, no_locale};
use super::{pg_test_pool_sized, unique_slug};
use crate::{
    core::{
        CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Registry,
        RelationshipConfig,
    },
    db::{
        DbConnection, DbPool, DbValue,
        migrate::sync_all,
        query::{
            self,
            ref_count::{after_create_from_data, before_hard_delete},
        },
    },
};

const WORKERS: usize = 8;
const ROUNDS: usize = 12;
const POOL_SIZE: u32 = 10;

/// The target collections and the ids every write points at.
struct Targets {
    users: String,
    media: String,
    tags: String,
}

/// A has-one relationship field `name` to `target`.
fn rel(name: &str, target: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Relationship)
        .relationship(RelationshipConfig::new(target, false))
        .build()
}

/// `posts` pointing at two users, two media and a tag.
fn posts_def(slug: &str, t: &Targets) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.fields = vec![
        rel("author", &t.users),
        rel("reviewer", &t.users),
        rel("cover", &t.media),
        rel("thumb", &t.media),
        rel("tag", &t.tags),
    ];

    def
}

/// Write data whose same-collection targets swap with `flip`, so half the
/// writers name them in the opposite order.
fn post_data(flip: bool) -> DocumentFields {
    let (a, b) = if flip { ("2", "1") } else { ("1", "2") };

    let mut data = DocumentFields::new();
    data.insert("author".to_string(), json!(format!("u{a}")));
    data.insert("reviewer".to_string(), json!(format!("u{b}")));
    data.insert("cover".to_string(), json!(format!("m{a}")));
    data.insert("thumb".to_string(), json!(format!("m{b}")));
    data.insert("tag".to_string(), json!("t1"));

    data
}

/// Create one post and count its references, in one transaction.
fn create_post(pool: &DbPool, def: &CollectionDefinition, flip: bool) -> Result<String> {
    let mut conn = pool.get()?;
    let tx = conn.transaction()?;

    let data = post_data(flip);
    let id = query::create(&tx, &def.slug, def, &data, None)?
        .id
        .to_string();
    after_create_from_data(&tx, &def.fields, &data, &no_locale())?;

    tx.commit()?;

    Ok(id)
}

/// Hard-delete one post and release its references, in one transaction.
fn delete_post(pool: &DbPool, def: &CollectionDefinition, id: &str) -> Result<()> {
    let mut conn = pool.get()?;
    let tx = conn.transaction()?;

    before_hard_delete(&tx, &def.slug, id, &def.fields, &no_locale())?;
    query::delete(&tx, &def.slug, id)?;

    tx.commit()?;

    Ok(())
}

/// One worker: create posts, hard-deleting every other one again. Returns the
/// errors it met.
fn work(pool: &DbPool, def: &CollectionDefinition, flip: bool) -> Vec<String> {
    let mut errors = Vec::new();

    for round in 0..ROUNDS {
        match create_post(pool, def, flip) {
            Ok(id) if round % 2 == 1 => {
                if let Err(e) = delete_post(pool, def, &id) {
                    errors.push(format!("{e:#}"));
                }
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("{e:#}")),
        }
    }

    errors
}

fn ref_count(conn: &dyn DbConnection, table: &str, id: &str) -> i64 {
    conn.query_one(
        &format!("SELECT _ref_count FROM \"{table}\" WHERE id = $1"),
        &[DbValue::Text(id.to_string())],
    )
    .unwrap()
    .unwrap()
    .get_i64("_ref_count")
    .unwrap()
}

/// Regression: the target row locks were taken per collection in hash-map
/// order — a fresh random order per write — so concurrent writes sharing
/// targets in several collections locked them crosswise and Postgres aborted
/// most of them with a deadlock (surfaced as a 503). Every target is now
/// locked in one pass sorted by collection and id, so none deadlock and every
/// count comes out exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_concurrent_writes_sharing_targets_never_deadlock() {
    let Some(pool) = pg_test_pool_sized(POOL_SIZE, None) else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let targets = Targets {
        users: unique_slug("rclusers"),
        media: unique_slug("rclmedia"),
        tags: unique_slug("rcltags"),
    };
    let def = posts_def(&unique_slug("rclposts"), &targets);

    let mut registry = Registry::new();
    for slug in [&targets.users, &targets.media, &targets.tags] {
        registry.register_collection(CollectionDefinition::new(slug.as_str()));
    }
    registry.register_collection(def.clone());
    sync_all(&pool, &registry, &no_locale()).expect("sync");

    let conn = pool.get().expect("conn");
    for (table, id) in [
        (&targets.users, "u1"),
        (&targets.users, "u2"),
        (&targets.media, "m1"),
        (&targets.media, "m2"),
        (&targets.tags, "t1"),
    ] {
        conn.execute(
            &format!("INSERT INTO \"{table}\" (id) VALUES ($1)"),
            &[DbValue::Text(id.to_string())],
        )
        .expect("insert target");
    }

    // Workers run on the runtime's blocking pool: the Postgres backend
    // drives its async client from there.
    let handles: Vec<_> = (0..WORKERS)
        .map(|n| {
            let (pool, def) = (pool.clone(), def.clone());
            spawn_blocking(move || work(&pool, &def, n % 2 == 1))
        })
        .collect();

    let mut errors: Vec<String> = Vec::new();
    for handle in handles {
        errors.extend(handle.await.expect("worker"));
    }

    let live = i64::try_from(WORKERS * ROUNDS.div_ceil(2)).expect("small");
    let counts = [
        ref_count(&conn, &targets.users, "u1"),
        ref_count(&conn, &targets.users, "u2"),
        ref_count(&conn, &targets.media, "m1"),
        ref_count(&conn, &targets.media, "m2"),
        ref_count(&conn, &targets.tags, "t1"),
    ];

    for slug in [
        def.slug.as_ref(),
        targets.users.as_str(),
        targets.media.as_str(),
        targets.tags.as_str(),
    ] {
        drop_tables_matching(&conn, slug);
    }

    assert!(errors.is_empty(), "no write may fail: {errors:?}");
    assert_eq!(counts, [live; 5], "every live post counts once per target");
}
