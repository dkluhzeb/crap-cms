//! Postgres harness: SQLSTATE classification, SQL composition of nested
//! JSON sources and reserved-word columns, and the cron window claim.

#![cfg(all(test, feature = "postgres"))]

use serde_json::json;

use super::{pg_test_pool, unique_slug};
use crate::{
    core::FieldType,
    db::{
        DbConnection, DbValue, FilterOp,
        query::{column_read_expr, filter::build_op_condition, jobs::try_claim_cron_window},
    },
    service::ServiceError,
};

/// A real 23505 and a real 23503 classify by SQLSTATE, not by message.
/// Postgres translates its messages per `lc_messages`, and the foreign-key
/// wording was previously mapped to `UniqueViolation` — a dangling
/// reference surfaced to clients as `ALREADY_EXISTS`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_constraint_violations_classify_by_sqlstate() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let parent = unique_slug("ckparent");
    let child = unique_slug("ckchild");
    let conn = pool.get().expect("conn");

    conn.execute(
        &format!("CREATE TABLE \"{parent}\" (id TEXT PRIMARY KEY, email TEXT UNIQUE)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!(
            "CREATE TABLE \"{child}\" (id TEXT PRIMARY KEY, \
             parent_id TEXT REFERENCES \"{parent}\"(id))"
        ),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!("INSERT INTO \"{parent}\" (id, email) VALUES ('p1', 'a@example.com')"),
        &[],
    )
    .unwrap();

    let duplicate = conn
        .execute(
            &format!("INSERT INTO \"{parent}\" (id, email) VALUES ('p2', 'a@example.com')"),
            &[],
        )
        .expect_err("duplicate email violates the unique index");
    assert!(
        matches!(
            ServiceError::classify(duplicate, "postgres"),
            ServiceError::UniqueViolation(_)
        ),
        "23505 must classify as a unique violation"
    );

    let dangling = conn
        .execute(
            &format!("INSERT INTO \"{child}\" (id, parent_id) VALUES ('c1', 'ghost')"),
            &[],
        )
        .expect_err("a reference to a missing parent violates the foreign key");
    assert!(
        matches!(
            ServiceError::classify(dangling, "postgres"),
            ServiceError::ForeignKeyViolation(_)
        ),
        "23503 must classify as a foreign-key violation, not as a unique one"
    );

    conn.execute(&format!("DROP TABLE \"{child}\""), &[])
        .unwrap();
    conn.execute(&format!("DROP TABLE \"{parent}\""), &[])
        .unwrap();
}

/// `json_each_source` has to accept a `json_extract_expr` as its source —
/// that is exactly how a filter descends into an array or blocks nested
/// inside a row. `#>>` yields `text` and Postgres has no implicit
/// text→jsonb cast, so without the cast the composed query fails with
/// "function `jsonb_array_elements_text(text)` does not exist".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_nested_json_each_source_runs_over_a_json_extract() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("jsoneach");
    let conn = pool.get().expect("conn");
    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY, data TEXT)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!("INSERT INTO \"{table}\" (id, data) VALUES ('r1', $1)"),
        &[DbValue::Text(json!({ "items": ["x", "y"] }).to_string())],
    )
    .unwrap();

    let source = conn.json_extract_expr(&format!("\"{table}\".data"), "items");
    let each = conn.json_each_source(&source, "e0");
    let sql = format!("SELECT COUNT(*) AS c FROM \"{table}\", {each}");

    let count = conn
        .query_one(&sql, &[])
        .expect("the nested each-source must compose with the extract")
        .unwrap()
        .get_i64("c")
        .unwrap();

    assert_eq!(count, 2, "both array elements expand into rows");

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// A column named after a SQL keyword compares against the column.
/// Unquoted, `user` on Postgres is the session-user function: the filter
/// silently matched nothing, and `array` / `only` were syntax errors.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_a_reserved_word_column_is_comparable() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let table = unique_slug("reserved");
    let conn = pool.get().expect("conn");
    conn.execute(
        &format!("CREATE TABLE \"{table}\" (id TEXT PRIMARY KEY, \"user\" TEXT)"),
        &[],
    )
    .unwrap();
    conn.execute(
        &format!("INSERT INTO \"{table}\" (id, \"user\") VALUES ('r1', 'alice')"),
        &[],
    )
    .unwrap();

    let mut params: Vec<DbValue> = Vec::new();
    let condition = build_op_condition(
        &conn,
        "user",
        &column_read_expr("user", &[], None).unwrap(),
        &FilterOp::Equals("alice".into()),
        Some(&FieldType::Text),
        &mut params,
    )
    .unwrap();

    let row = conn
        .query_one(
            &format!("SELECT id FROM \"{table}\" WHERE {condition}"),
            &params,
        )
        .expect("the comparand must name the column")
        .expect("the row matches");

    assert_eq!(row.get_string("id").unwrap(), "r1");

    conn.execute(&format!("DROP TABLE \"{table}\""), &[])
        .unwrap();
}

/// The cron window claim is one statement, so a second worker racing the
/// first fire of a slug backs off instead of failing on the primary key.
/// Postgres has no IMMEDIATE transaction — `transaction_immediate` is a
/// plain `BEGIN` at READ COMMITTED — so the two-statement form had a real
/// window here that `SQLite`'s write lock hid.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_the_cron_window_claim_is_atomic() {
    let Some(pool) = pg_test_pool() else {
        eprintln!("skipping: TEST_DATABASE_URL not set");
        return;
    };

    let conn = pool.get().expect("conn");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _crap_cron_fired (slug TEXT PRIMARY KEY, fired_at TEXT)",
        &[],
    )
    .unwrap();

    let slug = unique_slug("cron");

    assert!(
        try_claim_cron_window(&conn, &slug, "2026-01-01T00:05:00Z", "2026-01-01T00:00:00Z")
            .unwrap(),
        "the first claim of an unfired slug wins"
    );
    assert!(
        !try_claim_cron_window(&conn, &slug, "2026-01-01T00:05:01Z", "2026-01-01T00:00:00Z")
            .unwrap(),
        "a second claim in the same window backs off without erroring"
    );
    assert!(
        try_claim_cron_window(&conn, &slug, "2026-01-01T00:15:00Z", "2026-01-01T00:10:00Z")
            .unwrap(),
        "the next window claims again"
    );

    conn.execute(
        "DELETE FROM _crap_cron_fired WHERE slug = $1",
        &[DbValue::Text(slug)],
    )
    .unwrap();
}
