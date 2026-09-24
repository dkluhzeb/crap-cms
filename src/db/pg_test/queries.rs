//! Postgres harness: connectivity, array diffing, JSON paths, sorting and
//! row/advisory locks.

#![cfg(all(test, feature = "postgres"))]

use std::collections::HashMap;

use serde_json::json;

use super::{pg_test_pool, unique_slug};
use crate::{
    core::{FieldDefinition, FieldType},
    db::{
        DbConnection, DbValue, FilterOp,
        query::{
            filter::build_op_condition,
            join::{find_array_rows, set_array_rows},
        },
    },
};

/// Smoke test: prove the harness can connect to Postgres and round-trip a
/// value through the `DbConnection` trait. Skips when `TEST_DATABASE_URL`
/// is unset.
///
/// PG tests run on a multi-threaded Tokio runtime: the Postgres backend
/// bridges its async client to the sync `DbConnection` interface with
/// `block_in_place`, which panics outside a multi-thread runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_harness_connects_and_round_trips() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let table = unique_slug("smoke");

    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY, n INTEGER)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!("INSERT INTO \"{table}\" (id, n) VALUES ($1, $2)"),
        &[DbValue::Text("a".into()), DbValue::Integer(42)],
    )
    .unwrap();

    let row = conn
        .query_one(
            &format!("SELECT n FROM \"{table}\" WHERE id = $1"),
            &[DbValue::Text("a".into())],
        )
        .unwrap()
        .expect("row exists");
    assert_eq!(row.get_i64("n").unwrap(), 42);

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// Postgres: the diff-based array writer preserves a sub-field an update
/// omits (the row-identity fix), matching the `SQLite` unit coverage — proof
/// the standard-SQL diff behaves the same on both backends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_array_diff_preserves_omitted_column() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let slug = unique_slug("arrdiff");
    let table = format!("{slug}_items");

    conn.execute(
        &format!(
            "CREATE TABLE \"{table}\" \
             (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT, value TEXT)"
        ),
        &[],
    )
    .unwrap();

    let sub = vec![
        FieldDefinition::builder("label", FieldType::Text).build(),
        FieldDefinition::builder("value", FieldType::Text).build(),
    ];

    let rows = vec![
        HashMap::from([
            ("label".to_string(), json!("A")),
            ("value".to_string(), json!("va")),
        ]),
        HashMap::from([
            ("label".to_string(), json!("B")),
            ("value".to_string(), json!("vb")),
        ]),
    ];
    set_array_rows(&conn, &slug, "items", "p1", &rows, &sub, None).unwrap();

    let found = find_array_rows(&conn, &slug, "items", "p1", &sub, None).unwrap();
    let id0 = found[0]["id"].as_str().unwrap().to_string();

    // Update row 0 by id, changing `label`, omitting `value`; drop row 1.
    let update = vec![HashMap::from([
        ("id".to_string(), json!(id0)),
        ("label".to_string(), json!("A2")),
    ])];
    set_array_rows(&conn, &slug, "items", "p1", &update, &sub, None).unwrap();

    let after = find_array_rows(&conn, &slug, "items", "p1", &sub, None).unwrap();
    assert_eq!(after.len(), 1, "row absent from the update is deleted");
    assert_eq!(
        after[0]["id"].as_str().unwrap(),
        id0,
        "matched row keeps its id"
    );
    assert_eq!(after[0]["label"], "A2", "supplied column updated");
    assert_eq!(
        after[0]["value"], "va",
        "omitted column PRESERVED on Postgres"
    );

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// Regression: a filter on a Group sub-field nested inside a Blocks/Array
/// field reaches `json_extract_expr` with a DOTTED path (`meta.title`).
/// Postgres `->>` takes a single key, so `->>'meta.title'` looks for a
/// literal key named `meta.title` and returns NULL — the filter never
/// matches. The fix uses the `#>>'{meta,title}'` path form.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_json_extract_handles_nested_dot_path() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let table = unique_slug("jsonpath");

    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT, data JSONB)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!(
            "INSERT INTO \"{table}\" (id, data) VALUES ('x', '{{\"meta\":{{\"title\":\"hello\"}}}}')"
        ),
        &[],
    )
    .unwrap();

    let extract = conn.json_extract_expr("data", "meta.title");
    let row = conn
        .query_one(
            &format!("SELECT id FROM \"{table}\" WHERE {extract} = $1"),
            &[DbValue::Text("hello".into())],
        )
        .unwrap();
    assert!(
        row.is_some(),
        "a nested dot-path filter must match on Postgres (extract expr: {extract})"
    );

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// Regression: a `Number` sub-field extracted from JSON is `text` on
/// Postgres, so comparing it against a numeric operand (bound as float8)
/// errors (`operator does not exist: text > double precision`) or compares
/// lexically. `json_number_cast` wraps it so the comparison is numeric.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_json_number_subfield_compares_numerically() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let table = unique_slug("jsonnum");

    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT, data JSONB)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!(
            "INSERT INTO \"{table}\" (id, data) VALUES ('x', '{{\"meta\":{{\"price\":42}}}}')"
        ),
        &[],
    )
    .unwrap();

    let extract = conn.json_extract_expr("data", "meta.price");

    // Without the cast: text-vs-float8 comparison errors on Postgres.
    let raw = conn.query_one(
        &format!("SELECT id FROM \"{table}\" WHERE {extract} > $1"),
        &[DbValue::Real(9.0)],
    );
    assert!(
        raw.is_err(),
        "a text JSON extract compared to a numeric operand must error on PG (proves the bug)"
    );

    // With the cast: numeric comparison holds (42 > 9, and 42 = 42).
    let numeric = conn.json_number_cast(&extract);
    let gt = conn
        .query_one(
            &format!("SELECT id FROM \"{table}\" WHERE {numeric} > $1"),
            &[DbValue::Real(9.0)],
        )
        .unwrap();
    assert!(gt.is_some(), "42 > 9 must hold numerically after the cast");

    let eq = conn
        .query_one(
            &format!("SELECT id FROM \"{table}\" WHERE {numeric} = $1"),
            &[DbValue::Real(42.0)],
        )
        .unwrap();
    assert!(
        eq.is_some(),
        "numeric equality on a nested JSON Number must match"
    );

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// A `contains`/`like` filter on a numeric column must execute on Postgres
/// (no `bigint ~~ text` operator): the op builder casts the column to text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_pattern_filter_on_numeric_column_executes() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let table = unique_slug("numlike");
    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT, price DOUBLE PRECISION)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!("INSERT INTO \"{table}\" (id, price) VALUES ('a', 42), ('b', 7)"),
        &[],
    )
    .unwrap();

    let mut params = Vec::new();
    let condition = build_op_condition(
        &conn,
        "price",
        "price",
        &FilterOp::Contains("4".into()),
        Some(&FieldType::Number),
        &mut params,
    )
    .unwrap();
    let rows = conn
        .query_all(
            &format!("SELECT id FROM \"{table}\" WHERE {condition} ORDER BY id"),
            &params,
        )
        .expect("a pattern match on a numeric column must not be a backend error");
    let ids: Vec<String> = rows.iter().filter_map(|r| r.opt_text_at(0)).collect();
    assert_eq!(ids, vec!["a".to_string()]);

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// A bare-day operand on a date covers its whole UTC day on Postgres too: the
/// stored instants compare as text under the database's collation, so the
/// day's first and last minutes match and the next midnight does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_bare_day_date_filter_covers_the_day() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let table = unique_slug("dayfilter");
    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT, due TEXT)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!(
            "INSERT INTO \"{table}\" (id, due) VALUES \
             ('before', '2026-01-14T23:59:59.999Z'), ('early', '2026-01-15T00:30:00.000Z'), \
             ('late', '2026-01-15T23:59:00.000Z'), ('next', '2026-01-16T00:00:00.000Z'), \
             ('none', NULL)"
        ),
        &[],
    )
    .unwrap();

    let ids = |op: FilterOp| {
        let mut params = Vec::new();
        let condition = build_op_condition(
            &conn,
            "due",
            "due",
            &op,
            Some(&FieldType::Date),
            &mut params,
        )
        .unwrap();
        let rows = conn
            .query_all(
                &format!("SELECT id FROM \"{table}\" WHERE {condition} ORDER BY id"),
                &params,
            )
            .unwrap();

        rows.iter()
            .filter_map(|r| r.opt_text_at(0))
            .collect::<Vec<String>>()
    };

    let day = || "2026-01-15".to_string();
    assert_eq!(ids(FilterOp::Equals(day())), ["early", "late"]);
    assert_eq!(ids(FilterOp::NotEquals(day())), ["before", "next"]);
    assert_eq!(ids(FilterOp::GreaterThan(day())), ["next"]);
    assert_eq!(
        ids(FilterOp::LessThanOrEqual(day())),
        ["before", "early", "late"]
    );

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// Regression (root cause of the keyset dup/drop bug): Postgres defaults to
/// NULLs-LAST on ASC, the opposite of `SQLite` (NULLs-first) — and the keyset
/// clause assumes `SQLite`'s placement. The sort builder now emits an explicit
/// `NULLS FIRST` (ASC) / `NULLS LAST` (DESC), which PG must honor so its row
/// order matches `SQLite`'s and the keyset stays correct. This pins that PG
/// honors the clause (its default would order NULLs the other way).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_null_sort_places_nulls_per_explicit_clause() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("get PG connection");
    let table = unique_slug("nullsort");

    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT, sort_val INTEGER)"),
        &[],
    )
    .unwrap();
    for (id, val) in [("a", "NULL"), ("b", "5"), ("c", "NULL"), ("d", "3")] {
        conn.execute(
            &format!("INSERT INTO \"{table}\" (id, sort_val) VALUES ('{id}', {val})"),
            &[],
        )
        .unwrap();
    }

    let ids = |sql: String| -> Vec<String> {
        conn.query_all(&sql, &[])
            .unwrap()
            .iter()
            .map(|r| r.get_string("id").unwrap())
            .collect()
    };

    // PG's DEFAULT (no NULLS clause) on ASC puts NULLs LAST — the divergence
    // from SQLite (NULLs-first) that broke keyset pagination.
    assert_eq!(
        ids(format!(
            "SELECT id FROM \"{table}\" ORDER BY sort_val ASC, id ASC"
        )),
        vec!["d", "b", "a", "c"],
        "PG default ASC orders NULLs last (this is the SQLite divergence)"
    );

    // The explicit clause the sort builder now emits flips PG to NULLs-first
    // on ASC (matching SQLite) and NULLs-last on DESC.
    assert_eq!(
        ids(format!(
            "SELECT id FROM \"{table}\" ORDER BY sort_val ASC NULLS FIRST, id ASC"
        )),
        vec!["a", "c", "d", "b"],
        "explicit NULLS FIRST matches SQLite's ASC default"
    );
    assert_eq!(
        ids(format!(
            "SELECT id FROM \"{table}\" ORDER BY sort_val DESC NULLS LAST, id ASC"
        )),
        vec!["b", "d", "a", "c"],
        "explicit NULLS LAST matches SQLite's DESC default"
    );

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// Regression: the ref-count update path snapshots outgoing refs with an
/// UNLOCKED read before the document row is write-locked, so two concurrent
/// updates to one document could both read a stale `old_refs` under Postgres
/// MVCC and double-apply a delta (delete-protection bypass / phantom ref).
/// The fix locks the row first via `lock_row` (SELECT … FOR UPDATE). This
/// proves that lock is real and row-scoped: while one tx holds it, a
/// concurrent `FOR UPDATE NOWAIT` fails on that row but succeeds on another.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_lock_row_holds_a_real_row_lock() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("lockrow");
    {
        let setup = pool.get().expect("get PG connection");
        setup
            .execute(
                &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY)"),
                &[],
            )
            .unwrap();
        setup
            .execute(
                &format!("INSERT INTO \"{table}\" (id) VALUES ('x'), ('y')"),
                &[],
            )
            .unwrap();
    }

    let mut conn1 = pool.get().expect("conn1");
    let mut conn2 = pool.get().expect("conn2");

    let tx1 = conn1.transaction_immediate().unwrap();
    tx1.lock_row(&table, "x").unwrap();

    let tx2 = conn2.transaction_immediate().unwrap();

    // A row tx1 did NOT lock is still freely lockable (row-scoped).
    let free = tx2.query_one(
        &format!("SELECT 1 FROM \"{table}\" WHERE id='y' FOR UPDATE NOWAIT"),
        &[],
    );
    assert!(free.is_ok(), "an unlocked row must be lockable by tx2");

    // The row tx1 holds via lock_row cannot be locked concurrently.
    let locked = tx2.query_one(
        &format!("SELECT 1 FROM \"{table}\" WHERE id='x' FOR UPDATE NOWAIT"),
        &[],
    );
    assert!(
        locked.is_err(),
        "row 'x' held by tx1's lock_row must block a concurrent FOR UPDATE"
    );

    drop(tx2);
    drop(tx1);

    let cleanup = pool.get().expect("cleanup conn");
    cleanup
        .execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// Regression: `advisory_xact_lock` serializes a critical section across
/// connections — the mechanism that makes per-slug/per-queue job caps exact
/// cluster-wide on Postgres (two nodes' concurrent claims can't both pass a
/// cap check that misses the other's in-flight `running` rows). While one
/// transaction holds the key, a peer cannot acquire it; once released, it can.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_advisory_xact_lock_is_mutually_exclusive_across_connections() {
    const KEY: i64 = 424_242; // arbitrary key, distinct from JOB_CLAIM_LOCK_KEY

    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let mut conn1 = pool.get().expect("conn1");
    let mut conn2 = pool.get().expect("conn2");

    let tx1 = conn1.transaction_immediate().unwrap();
    tx1.advisory_xact_lock(KEY).unwrap();

    // A peer cannot acquire the same key while tx1 holds it.
    let try_acquire = |conn: &dyn DbConnection| -> i64 {
        conn.query_one(
            &format!(
                "SELECT pg_try_advisory_xact_lock({})::int AS got",
                conn.placeholder(1)
            ),
            &[DbValue::Integer(KEY)],
        )
        .unwrap()
        .unwrap()
        .get_i64("got")
        .unwrap()
    };

    let tx2 = conn2.transaction_immediate().unwrap();
    assert_eq!(
        try_acquire(&tx2),
        0,
        "the key is held by tx1 — a peer must fail"
    );
    drop(tx2);

    // Once tx1 releases the transaction-scoped lock, the key is free again.
    drop(tx1);
    let tx3 = conn2.transaction_immediate().unwrap();
    assert_eq!(
        try_acquire(&tx3),
        1,
        "after tx1 releases, the key is acquirable"
    );
    drop(tx3);
}
