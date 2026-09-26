//! Postgres harness: a delete or trash judges its access rule's row
//! constraints on the row it removes, not one a concurrent writer changed in
//! between.

#![cfg(all(test, feature = "postgres"))]

use std::{thread, time::Duration};

use anyhow::Result;
use serde_json::json;
use tokio::task::spawn_blocking;

use super::support::{drop_tables_matching, no_locale, row_count};
use super::{pg_test_pool_sized, unique_slug};
use crate::{
    core::{
        CollectionDefinition, DocumentFields, FieldDefinition, FieldType, Hooks, Registry,
        ValidationError,
    },
    db::{
        AccessResult, DbConnection, DbPool, Filter, FilterClause, FilterOp, migrate::sync_all,
        query,
    },
    hooks::{AccessCheckInput, HookContext, HookEvent, ValidationCtx},
    service::{FieldReadStrip, ServiceContext, ServiceError, WriteHooks, delete_document},
};

/// Write hooks whose every access rule is "only documents authored by `E`".
struct AuthorEOnly;

impl WriteHooks for AuthorEOnly {
    fn run_before_write(
        &self,
        _hooks: &Hooks,
        _fields: &[FieldDefinition],
        ctx: HookContext,
        _val_ctx: &ValidationCtx,
    ) -> Result<HookContext> {
        Ok(ctx)
    }

    fn run_after_write(
        &self,
        _hooks: &Hooks,
        _fields: &[FieldDefinition],
        _event: HookEvent,
        ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        Ok(ctx)
    }

    fn run_hooks_with_conn(
        &self,
        _hooks: &Hooks,
        _event: HookEvent,
        ctx: HookContext,
        _conn: &dyn DbConnection,
    ) -> Result<HookContext> {
        Ok(ctx)
    }

    fn check_access(&self, _input: &AccessCheckInput<'_>) -> Result<AccessResult> {
        Ok(AccessResult::Constrained(vec![FilterClause::Single(
            Filter {
                field: "author".to_string(),
                op: FilterOp::Equals("E".to_string()),
            },
        )]))
    }

    fn validate_fields(
        &self,
        _fields: &[FieldDefinition],
        _data: &DocumentFields,
        _ctx: &ValidationCtx,
    ) -> std::result::Result<(), ValidationError> {
        Ok(())
    }
}

impl FieldReadStrip for AuthorEOnly {}

/// A collection with an `author` text field, trashable when `trash`.
fn posts_def(slug: &str, trash: bool) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.soft_delete = trash;
    def.fields = vec![FieldDefinition::builder("author", FieldType::Text).build()];

    def
}

fn author(value: &str) -> DocumentFields {
    let mut data = DocumentFields::new();
    data.insert("author".to_string(), json!(value));

    data
}

/// Delete `id` in its own transaction under [`AuthorEOnly`], committing on
/// success.
fn delete_in_own_tx(
    pool: &DbPool,
    def: &CollectionDefinition,
    id: &str,
) -> std::result::Result<(), ServiceError> {
    let mut conn = pool.get().expect("conn");
    let tx = conn.transaction().expect("tx");
    let hooks = AuthorEOnly;

    let ctx = ServiceContext::collection(&def.slug, def)
        .conn(&tx)
        .write_hooks(&hooks)
        .build();
    delete_document(&ctx, id, None, None)?;

    tx.commit().expect("commit");

    Ok(())
}

/// Rows outside the trash.
fn live_rows(conn: &dyn DbConnection, slug: &str, trash: bool) -> i64 {
    if !trash {
        return row_count(conn, slug);
    }

    conn.query_one(
        &format!("SELECT COUNT(*) AS c FROM \"{slug}\" WHERE _deleted_at IS NULL"),
        &[],
    )
    .unwrap()
    .unwrap()
    .get_i64("c")
    .unwrap()
}

/// Race one delete (or trash) of an `E`-authored document against a writer
/// that reassigns it to `X`, and report the delete's outcome and how many
/// live rows remain.
async fn race_delete_against_reassign(trash: bool) -> Option<(bool, i64)> {
    let pool = pg_test_pool_sized(4, None)?;

    let slug = unique_slug(if trash { "trashlock" } else { "dellock" });
    let def = posts_def(&slug, trash);

    let mut registry = Registry::new();
    registry.register_collection(def.clone());
    sync_all(&pool, &registry, &no_locale()).expect("sync");

    let id = {
        let conn = pool.get().expect("conn");
        query::create(&conn, &slug, &def, &author("E"), None)
            .expect("create")
            .id
            .to_string()
    };

    // The writer holds the row lock while it reassigns the document.
    let mut writer_conn = pool.get().expect("conn");
    let writer = writer_conn.transaction().expect("tx");
    writer.lock_row(&slug, &id).expect("lock");

    let deleter = {
        let (pool, def, id) = (pool.clone(), def.clone(), id.clone());
        spawn_blocking(move || delete_in_own_tx(&pool, &def, &id))
    };

    // Give the delete time to reach the row before the writer commits.
    thread::sleep(Duration::from_millis(300));

    query::update(&writer, &slug, &def, &id, &author("X"), None).expect("update");
    writer.commit().expect("commit");

    let outcome = deleter.await.expect("delete task");

    let conn = pool.get().expect("conn");
    let live = live_rows(&conn, &slug, trash);

    drop_tables_matching(&conn, &slug);

    Some((matches!(outcome, Err(ServiceError::AccessDenied(_))), live))
}

/// Regression: a delete judged `access.delete`'s row constraints before it
/// took any lock, so a writer reassigning the document in between let the
/// delete remove a document the rule no longer admitted. Locking before the
/// judgment makes the delete wait for the writer and judge what it
/// committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_delete_judges_the_row_a_concurrent_writer_committed() {
    let Some((denied, live)) = race_delete_against_reassign(false).await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    assert!(denied, "the reassigned document is no longer deletable");
    assert_eq!(live, 1, "the document survives");
}

/// The trash twin of [`pg_delete_judges_the_row_a_concurrent_writer_committed`]:
/// `access.trash` is judged on the locked row too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_trash_judges_the_row_a_concurrent_writer_committed() {
    let Some((denied, live)) = race_delete_against_reassign(true).await else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    assert!(denied, "the reassigned document is no longer trashable");
    assert_eq!(live, 1, "the document stays out of the trash");
}
