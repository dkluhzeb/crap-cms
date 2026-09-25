//! Running one `SQLite` statement: binding parameters, reading rows, and the
//! statement budget every statement runs under (see [`crate::db::deadline`]).

use std::str::from_utf8;

use anyhow::{Context as _, Result, bail};
use rusqlite::{
    Connection, Row,
    types::{Null, ToSql, ValueRef},
};
use tracing::warn;

use crate::db::{
    DbRow, DbValue,
    deadline::{StatementRun, StatementTimedOut},
};

/// Why a statement or commit is refused after the database ended its
/// transaction on its own.
pub(super) const TRANSACTION_LOST: &str = "the transaction was rolled back, not committed: SQLite \
     ended it when a statement inside it was interrupted or failed (a time limit, a full disk, \
     an I/O error), so nothing more runs in it";

/// Refuse a statement on a transaction the database already ended (`lost`):
/// run in autocommit, it would commit on its own while the transaction's
/// earlier writes are gone.
///
/// # Errors
///
/// Returns an error when `lost` is set.
pub(super) fn ensure_transaction_alive(lost: bool) -> Result<()> {
    if lost {
        bail!(TRANSACTION_LOST);
    }

    Ok(())
}

/// Run one statement under the thread's statement budget, marking its error
/// as a timeout when the progress handler interrupted it for running out of
/// time.
pub(super) fn timed_run<T>(statement: impl FnOnce() -> Result<T>) -> Result<T> {
    let _run = StatementRun::start();
    let result = statement();

    if result.is_ok() || !StatementRun::timed_out() {
        return result;
    }

    result.map_err(|e| e.context(StatementTimedOut))
}

/// Execute a statement that modifies data.
pub(super) fn sqlite_execute(inner: &Connection, sql: &str, params: &[DbValue]) -> Result<usize> {
    let rusqlite_params = to_rusqlite_params(params);
    let refs: Vec<&dyn ToSql> = rusqlite_params.iter().map(AsRef::as_ref).collect();

    // `prepare_cached`, like the read methods: `Connection::execute`
    // prepares from scratch every call, and re-running
    // `sqlite3_prepare_v2` takes SQLite's globally-locked allocator —
    // the contention the `stmt_cache_capacity` knob exists to avoid.
    // A statement that returns rows is rejected by `Statement::execute`
    // on either path, so no read reaches here (`RETURNING` goes through
    // `query_one`).
    Connection::prepare_cached(inner, sql)
        .with_context(|| format!("prepare failed: {sql}"))?
        .execute(refs.as_slice())
        .with_context(|| format!("execute failed: {sql}"))
}

/// Run a query and collect every row.
pub(super) fn sqlite_query_all(
    inner: &Connection,
    sql: &str,
    params: &[DbValue],
) -> Result<Vec<DbRow>> {
    let rusqlite_params = to_rusqlite_params(params);
    let refs: Vec<&dyn ToSql> = rusqlite_params.iter().map(AsRef::as_ref).collect();

    let mut stmt =
        Connection::prepare_cached(inner, sql).with_context(|| format!("prepare failed: {sql}"))?;

    let col_count = stmt.column_count();
    let col_names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("").to_string())
        .collect();

    let rows = stmt
        .query_map(refs.as_slice(), |row| {
            Ok(rusqlite_row_to_dbrow(row, col_count, &col_names))
        })
        .with_context(|| format!("query_map failed: {sql}"))?;

    let mut result = Vec::new();

    for row in rows {
        result.push(row.context("failed to read row")?);
    }

    Ok(result)
}

/// Run a query and return its first row.
pub(super) fn sqlite_query_one(
    inner: &Connection,
    sql: &str,
    params: &[DbValue],
) -> Result<Option<DbRow>> {
    let rusqlite_params = to_rusqlite_params(params);
    let refs: Vec<&dyn ToSql> = rusqlite_params.iter().map(AsRef::as_ref).collect();

    let mut stmt =
        Connection::prepare_cached(inner, sql).with_context(|| format!("prepare failed: {sql}"))?;

    let col_count = stmt.column_count();
    let col_names: Vec<String> = (0..col_count)
        .map(|i| stmt.column_name(i).unwrap_or("").to_string())
        .collect();

    let mut rows = stmt
        .query_map(refs.as_slice(), |row| {
            Ok(rusqlite_row_to_dbrow(row, col_count, &col_names))
        })
        .with_context(|| format!("query_map failed: {sql}"))?;

    match rows.next() {
        Some(row) => Ok(Some(row.context("failed to read row")?)),
        None => Ok(None),
    }
}

/// Convert `&[DbValue]` to a `Vec<Box<dyn ToSql>>` for rusqlite.
fn to_rusqlite_params(params: &[DbValue]) -> Vec<Box<dyn ToSql>> {
    params
        .iter()
        .map(|v| -> Box<dyn ToSql> {
            match v {
                DbValue::Null => Box::new(Null),
                DbValue::Integer(i) => Box::new(*i),
                DbValue::Real(f) => Box::new(*f),
                DbValue::Text(s) => Box::new(s.clone()),
                DbValue::Blob(b) => Box::new(b.clone()),
            }
        })
        .collect()
}

/// Convert a `rusqlite::Row` to a `DbRow`.
fn rusqlite_row_to_dbrow(row: &Row, col_count: usize, col_names: &[String]) -> DbRow {
    let mut values = Vec::with_capacity(col_count);

    for i in 0..col_count {
        let val = row.get_ref(i).map_or(DbValue::Null, |v| match v {
            ValueRef::Null => DbValue::Null,
            ValueRef::Integer(i) => DbValue::Integer(i),
            ValueRef::Real(f) => DbValue::Real(f),
            ValueRef::Text(s) => match from_utf8(s) {
                Ok(valid) => DbValue::Text(valid.to_owned()),
                Err(e) => {
                    warn!("Invalid UTF-8 in SQLite text column: {}", e);

                    DbValue::Text(String::from_utf8_lossy(s).into_owned())
                }
            },
            ValueRef::Blob(b) => DbValue::Blob(b.to_vec()),
        });
        values.push(val);
    }

    DbRow::new(col_names.to_vec(), values)
}
