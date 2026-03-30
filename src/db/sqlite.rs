//! SQLite implementation of `DbConnection`.

use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
};

use anyhow::{Context as _, Result, bail};
use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::types::Value as SqliteValue;

use crate::core::FieldType;

use super::{
    connection::{ConnectionInner, DbConnection, TransactionInner},
    types::{DbRow, DbValue},
};

// ── Shared SQLite dialect helpers ────────────────────────────────────────

fn sqlite_table_exists(conn: &dyn DbConnection, name: &str) -> Result<bool> {
    let row = conn.query_one(
        "SELECT COUNT(*) AS cnt FROM sqlite_master WHERE type='table' AND name=?1",
        &[DbValue::Text(name.to_string())],
    )?;
    Ok(row.map(|r| r.get_i64("cnt").unwrap_or(0)).unwrap_or(0) > 0)
}

fn sqlite_get_table_columns(conn: &dyn DbConnection, table: &str) -> Result<HashSet<String>> {
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("Invalid table name for PRAGMA: {:?}", table);
    }
    let rows = conn.query_all(&format!("PRAGMA table_info({})", table), &[])?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.get_string("name").ok())
        .collect())
}

fn sqlite_get_table_column_types(
    conn: &dyn DbConnection,
    table: &str,
) -> Result<HashMap<String, String>> {
    if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("Invalid table name for PRAGMA: {:?}", table);
    }
    let rows = conn.query_all(&format!("PRAGMA table_info({})", table), &[])?;
    let mut map = HashMap::new();
    for row in rows {
        if let (Ok(name), Ok(col_type)) = (row.get_string("name"), row.get_string("type")) {
            map.insert(name, col_type);
        }
    }
    Ok(map)
}

fn sqlite_index_names(conn: &dyn DbConnection, table: &str, prefix: &str) -> Result<Vec<String>> {
    let rows = conn.query_all(
        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name=?1 AND name LIKE ?2",
        &[
            DbValue::Text(table.to_string()),
            DbValue::Text(format!("{}%", prefix)),
        ],
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.get_string("name").ok())
        .collect())
}

fn sqlite_column_type_for(ft: &FieldType) -> &'static str {
    sqlite_column_type_for_field(ft)
}

fn sqlite_date_offset_expr(seconds: i64, param_pos: usize) -> (String, DbValue) {
    // Use the absolute value with explicit sign to avoid double-negation
    // when a negative value is passed (e.g., -30 would produce "--30 seconds").
    let abs = seconds.unsigned_abs();
    let sign = if seconds >= 0 { "-" } else { "+" };
    (
        format!("datetime('now', ?{})", param_pos),
        DbValue::Text(format!("{}{} seconds", sign, abs)),
    )
}

fn sqlite_list_user_tables(conn: &dyn DbConnection) -> Result<Vec<String>> {
    let rows = conn.query_all(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        &[],
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|r| r.get_string("name").ok())
        .collect())
}

fn sqlite_supports_drop_column(conn: &dyn DbConnection) -> bool {
    let version = conn
        .query_one("SELECT sqlite_version() AS v", &[])
        .ok()
        .flatten()
        .and_then(|row| row.get_string("v").ok())
        .unwrap_or_default();
    let parts: Vec<u32> = version.split('.').filter_map(|s| s.parse().ok()).collect();
    parts.len() >= 2 && (parts[0] > 3 || (parts[0] == 3 && parts[1] >= 35))
}

fn sqlite_vacuum_into(conn: &dyn DbConnection, dest: &std::path::Path) -> Result<()> {
    let p1 = conn.placeholder(1);
    conn.execute(
        &format!("VACUUM INTO {p1}"),
        &[DbValue::Text(dest.to_string_lossy().into_owned())],
    )?;
    Ok(())
}

const SQLITE_SIDECAR_EXTENSIONS: &[&str] = &["db-wal", "db-shm"];

/// Normalize SQLite's `"YYYY-MM-DD HH:MM:SS"` to ISO 8601 `"YYYY-MM-DDTHH:MM:SS.000Z"`.
/// Already-normalized values pass through unchanged.
fn sqlite_normalize_timestamp(ts: &str) -> String {
    if ts.len() == 19
        && ts.as_bytes().get(10) == Some(&b' ')
        && ts.is_char_boundary(10)
        && ts.is_char_boundary(11)
    {
        format!("{}T{}.000Z", &ts[..10], &ts[11..])
    } else {
        ts.to_string()
    }
}

fn sqlite_column_type_for_field(ft: &FieldType) -> &'static str {
    match ft {
        FieldType::Number => "REAL",
        FieldType::Checkbox => "INTEGER",
        _ => "TEXT",
    }
}

fn sqlite_build_insert_ignore(table: &str, columns: &str, values: &str) -> String {
    format!(
        "INSERT OR IGNORE INTO {} ({}) VALUES ({})",
        table, columns, values
    )
}

fn sqlite_build_upsert(table: &str, columns: &[&str], values: &str, _key_col: &str) -> String {
    let cols = columns
        .iter()
        .map(|c| format!("\"{}\"", c))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT OR REPLACE INTO {} ({}) VALUES ({})",
        table, cols, values
    )
}

/// Wraps a pooled `rusqlite` connection, implementing `DbConnection`.
pub struct SqliteConnection {
    inner: PooledConnection<SqliteConnectionManager>,
}

impl Deref for SqliteConnection {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &rusqlite::Connection {
        &self.inner
    }
}

impl SqliteConnection {
    /// Wrap a pooled connection.
    pub fn new(inner: PooledConnection<SqliteConnectionManager>) -> Self {
        Self { inner }
    }

    /// Open an `IMMEDIATE` transaction (write-lock from the start).
    pub fn transaction_immediate(&mut self) -> Result<SqliteTransaction<'_>> {
        let tx = self
            .inner
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .context("Failed to begin IMMEDIATE transaction")?;
        Ok(SqliteTransaction::new(tx))
    }

    /// Open a `DEFERRED` transaction (default behavior).
    pub fn transaction(&mut self) -> Result<SqliteTransaction<'_>> {
        let tx = self
            .inner
            .transaction()
            .context("Failed to begin transaction")?;
        Ok(SqliteTransaction::new(tx))
    }
}

impl ConnectionInner for SqliteConnection {
    fn transaction_boxed(&mut self) -> Result<Box<dyn TransactionInner + '_>> {
        let tx = self.transaction()?;
        Ok(Box::new(tx))
    }

    fn transaction_immediate_boxed(&mut self) -> Result<Box<dyn TransactionInner + '_>> {
        let tx = self.transaction_immediate()?;
        Ok(Box::new(tx))
    }
}

impl DbConnection for SqliteConnection {
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();
        let count = self
            .inner
            .execute(sql, refs.as_slice())
            .with_context(|| format!("execute failed: {sql}"))?;
        Ok(count)
    }

    fn execute_batch(&self, sql: &str) -> Result<()> {
        self.inner
            .execute_batch(sql)
            .with_context(|| format!("execute_batch failed: {sql}"))?;
        Ok(())
    }

    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self
            .inner
            .prepare(sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self
            .inner
            .prepare(sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn placeholder(&self, n: usize) -> String {
        format!("?{n}")
    }

    fn now_expr(&self) -> &'static str {
        "datetime('now')"
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        format!("MAX({a}, {b})")
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        sqlite_table_exists(self, name)
    }

    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
        sqlite_get_table_columns(self, table)
    }

    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
        sqlite_get_table_column_types(self, table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
        sqlite_index_names(self, table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        "TEXT DEFAULT (datetime('now'))"
    }

    fn timestamp_column_type(&self) -> &'static str {
        "TEXT"
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        sqlite_column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        sqlite_date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        format!("json_extract({}, '$.{}')", column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        format!("json_each({}) AS {}", source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        sqlite_build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        sqlite_build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        true
    }

    fn like_operator(&self) -> &'static str {
        "LIKE"
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        sqlite_list_user_tables(self)
    }

    fn supports_drop_column(&self) -> bool {
        sqlite_supports_drop_column(self)
    }

    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()> {
        sqlite_vacuum_into(self, dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        SQLITE_SIDECAR_EXTENSIONS
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        sqlite_normalize_timestamp(ts)
    }
}

/// Wraps a `rusqlite::Transaction`, implementing `DbConnection`.
pub struct SqliteTransaction<'conn> {
    inner: rusqlite::Transaction<'conn>,
}

impl<'conn> SqliteTransaction<'conn> {
    /// Wrap a rusqlite transaction.
    fn new(inner: rusqlite::Transaction<'conn>) -> Self {
        Self { inner }
    }

    /// Commit this transaction.
    pub fn commit(self) -> Result<()> {
        self.inner.commit().context("Failed to commit transaction")
    }
}

impl TransactionInner for SqliteTransaction<'_> {
    fn commit_inner(self: Box<Self>) -> Result<()> {
        (*self).commit()
    }
}

impl DbConnection for SqliteTransaction<'_> {
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();
        // Use UFCS to avoid ambiguity: `SqliteTransaction` contains a `rusqlite::Transaction`,
        // which now also implements `DbConnection`. Deref to `rusqlite::Connection` explicitly
        // and call the inherent method to resolve the ambiguity.
        let inner: &rusqlite::Connection = std::ops::Deref::deref(&self.inner);
        let count = rusqlite::Connection::execute(inner, sql, refs.as_slice())
            .with_context(|| format!("execute failed: {sql}"))?;
        Ok(count)
    }

    fn execute_batch(&self, sql: &str) -> Result<()> {
        let inner: &rusqlite::Connection = std::ops::Deref::deref(&self.inner);
        rusqlite::Connection::execute_batch(inner, sql)
            .with_context(|| format!("execute_batch failed: {sql}"))?;
        Ok(())
    }

    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self
            .inner
            .prepare(sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self
            .inner
            .prepare(sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn placeholder(&self, n: usize) -> String {
        format!("?{n}")
    }

    fn now_expr(&self) -> &'static str {
        "datetime('now')"
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        format!("MAX({a}, {b})")
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        sqlite_table_exists(self, name)
    }

    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
        sqlite_get_table_columns(self, table)
    }

    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
        sqlite_get_table_column_types(self, table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
        sqlite_index_names(self, table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        "TEXT DEFAULT (datetime('now'))"
    }

    fn timestamp_column_type(&self) -> &'static str {
        "TEXT"
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        sqlite_column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        sqlite_date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        format!("json_extract({}, '$.{}')", column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        format!("json_each({}) AS {}", source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        sqlite_build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        sqlite_build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        true
    }

    fn like_operator(&self) -> &'static str {
        "LIKE"
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        sqlite_list_user_tables(self)
    }

    fn supports_drop_column(&self) -> bool {
        sqlite_supports_drop_column(self)
    }

    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()> {
        sqlite_vacuum_into(self, dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        SQLITE_SIDECAR_EXTENSIONS
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        sqlite_normalize_timestamp(ts)
    }
}

/// Implement `DbConnection` for `rusqlite::Transaction` so that callers that open
/// a raw `rusqlite::Transaction` (e.g. via `pool.get()?.transaction()`) can pass `&tx`
/// wherever `&dyn DbConnection` is expected without wrapping in `SqliteTransaction`.
///
/// `rusqlite::Transaction<'_>: Deref<Target = rusqlite::Connection>`, so we deref
/// explicitly via `std::ops::Deref::deref(self)` to reach the inherent methods and
/// avoid ambiguity with our trait methods that share the same name.
impl DbConnection for rusqlite::Transaction<'_> {
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        let inner: &rusqlite::Connection = std::ops::Deref::deref(self);
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();
        let count = rusqlite::Connection::execute(inner, sql, refs.as_slice())
            .with_context(|| format!("execute failed: {sql}"))?;
        Ok(count)
    }

    fn execute_batch(&self, sql: &str) -> Result<()> {
        let inner: &rusqlite::Connection = std::ops::Deref::deref(self);
        rusqlite::Connection::execute_batch(inner, sql)
            .with_context(|| format!("execute_batch failed: {sql}"))?;
        Ok(())
    }

    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
        let inner: &rusqlite::Connection = std::ops::Deref::deref(self);
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = rusqlite::Connection::prepare(inner, sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
        let inner: &rusqlite::Connection = std::ops::Deref::deref(self);
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = rusqlite::Connection::prepare(inner, sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn placeholder(&self, n: usize) -> String {
        format!("?{n}")
    }

    fn now_expr(&self) -> &'static str {
        "datetime('now')"
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        format!("MAX({a}, {b})")
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        sqlite_table_exists(self, name)
    }

    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
        sqlite_get_table_columns(self, table)
    }

    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
        sqlite_get_table_column_types(self, table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
        sqlite_index_names(self, table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        "TEXT DEFAULT (datetime('now'))"
    }

    fn timestamp_column_type(&self) -> &'static str {
        "TEXT"
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        sqlite_column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        sqlite_date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        format!("json_extract({}, '$.{}')", column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        format!("json_each({}) AS {}", source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        sqlite_build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        sqlite_build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        true
    }

    fn like_operator(&self) -> &'static str {
        "LIKE"
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        sqlite_list_user_tables(self)
    }

    fn supports_drop_column(&self) -> bool {
        sqlite_supports_drop_column(self)
    }

    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()> {
        sqlite_vacuum_into(self, dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        SQLITE_SIDECAR_EXTENSIONS
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        sqlite_normalize_timestamp(ts)
    }
}

/// Implement `DbConnection` directly on `rusqlite::Connection` for test convenience.
/// This lets `#[cfg(test)]` code open an in-memory connection and pass it to
/// functions that accept `&dyn DbConnection` without a wrapper type.
///
/// All method calls use UFCS (`rusqlite::Connection::method(self, ...)`) to avoid
/// ambiguity with the trait methods that share the same name.
impl DbConnection for rusqlite::Connection {
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();
        let count = rusqlite::Connection::execute(self, sql, refs.as_slice())
            .with_context(|| format!("execute failed: {sql}"))?;
        Ok(count)
    }

    fn execute_batch(&self, sql: &str) -> Result<()> {
        rusqlite::Connection::execute_batch(self, sql)
            .with_context(|| format!("execute_batch failed: {sql}"))?;
        Ok(())
    }

    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = rusqlite::Connection::prepare(self, sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
        let rusqlite_params = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            rusqlite_params.iter().map(|b| b.as_ref()).collect();

        let mut stmt = rusqlite::Connection::prepare(self, sql)
            .with_context(|| format!("prepare failed: {sql}"))?;

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

    fn placeholder(&self, n: usize) -> String {
        format!("?{n}")
    }

    fn now_expr(&self) -> &'static str {
        "datetime('now')"
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        format!("MAX({a}, {b})")
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        sqlite_table_exists(self, name)
    }

    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
        sqlite_get_table_columns(self, table)
    }

    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
        sqlite_get_table_column_types(self, table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
        sqlite_index_names(self, table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        "TEXT DEFAULT (datetime('now'))"
    }

    fn timestamp_column_type(&self) -> &'static str {
        "TEXT"
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        sqlite_column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        sqlite_date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        format!("json_extract({}, '$.{}')", column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        format!("json_each({}) AS {}", source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        sqlite_build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        sqlite_build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        true
    }

    fn like_operator(&self) -> &'static str {
        "LIKE"
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        sqlite_list_user_tables(self)
    }

    fn supports_drop_column(&self) -> bool {
        sqlite_supports_drop_column(self)
    }

    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()> {
        sqlite_vacuum_into(self, dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        SQLITE_SIDECAR_EXTENSIONS
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        sqlite_normalize_timestamp(ts)
    }
}

/// Convert `&[DbValue]` to a `Vec<Box<dyn ToSql>>` for rusqlite.
fn to_rusqlite_params(params: &[DbValue]) -> Vec<Box<dyn rusqlite::types::ToSql>> {
    params
        .iter()
        .map(|v| -> Box<dyn rusqlite::types::ToSql> {
            match v {
                DbValue::Null => Box::new(rusqlite::types::Null),
                DbValue::Integer(i) => Box::new(*i),
                DbValue::Real(f) => Box::new(*f),
                DbValue::Text(s) => Box::new(s.clone()),
                DbValue::Blob(b) => Box::new(b.clone()),
            }
        })
        .collect()
}

/// Convert a `rusqlite::Row` to a `DbRow`.
fn rusqlite_row_to_dbrow(row: &rusqlite::Row, col_count: usize, col_names: &[String]) -> DbRow {
    let mut values = Vec::with_capacity(col_count);
    for i in 0..col_count {
        let val = row
            .get_ref(i)
            .map(|v| match v {
                rusqlite::types::ValueRef::Null => DbValue::Null,
                rusqlite::types::ValueRef::Integer(i) => DbValue::Integer(i),
                rusqlite::types::ValueRef::Real(f) => DbValue::Real(f),
                rusqlite::types::ValueRef::Text(s) => match std::str::from_utf8(s) {
                    Ok(valid) => DbValue::Text(valid.to_owned()),
                    Err(e) => {
                        tracing::warn!("Invalid UTF-8 in SQLite text column: {}", e);
                        DbValue::Text(String::from_utf8_lossy(s).into_owned())
                    }
                },
                rusqlite::types::ValueRef::Blob(b) => DbValue::Blob(b.to_vec()),
            })
            .unwrap_or(DbValue::Null);
        values.push(val);
    }
    DbRow::new(col_names.to_vec(), values)
}

/// Convert a `rusqlite::types::Value` to a `DbValue`.
pub fn from_sqlite_value(val: &SqliteValue) -> DbValue {
    match val {
        SqliteValue::Null => DbValue::Null,
        SqliteValue::Integer(i) => DbValue::Integer(*i),
        SqliteValue::Real(f) => DbValue::Real(*f),
        SqliteValue::Text(s) => DbValue::Text(s.clone()),
        SqliteValue::Blob(b) => DbValue::Blob(b.clone()),
    }
}

/// Thin wrapper around `rusqlite::Connection` that implements `DbConnection`.
/// Used in unit tests to create in-memory connections without an r2d2 pool.
#[cfg(test)]
pub struct InMemoryConn(pub rusqlite::Connection);

#[cfg(test)]
impl InMemoryConn {
    /// Open an in-memory SQLite database.
    pub fn open() -> Self {
        Self(rusqlite::Connection::open_in_memory().unwrap())
    }

    /// Execute a batch of SQL statements (test helper).
    pub fn setup(&self, sql: &str) {
        self.0.execute_batch(sql).unwrap();
    }
}

#[cfg(test)]
impl super::connection::DbConnection for InMemoryConn {
    fn execute(&self, sql: &str, params: &[super::types::DbValue]) -> anyhow::Result<usize> {
        let p = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> = p.iter().map(|b| b.as_ref()).collect();
        Ok(self.0.execute(sql, refs.as_slice())?)
    }

    fn execute_batch(&self, sql: &str) -> anyhow::Result<()> {
        Ok(self.0.execute_batch(sql)?)
    }

    fn query_all(
        &self,
        sql: &str,
        params: &[super::types::DbValue],
    ) -> anyhow::Result<Vec<super::types::DbRow>> {
        let p = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> = p.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.0.prepare(sql)?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("").to_string())
            .collect();
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok(rusqlite_row_to_dbrow(row, col_count, &col_names))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }

    fn query_one(
        &self,
        sql: &str,
        params: &[super::types::DbValue],
    ) -> anyhow::Result<Option<super::types::DbRow>> {
        let p = to_rusqlite_params(params);
        let refs: Vec<&dyn rusqlite::types::ToSql> = p.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.0.prepare(sql)?;
        let col_count = stmt.column_count();
        let col_names: Vec<String> = (0..col_count)
            .map(|i| stmt.column_name(i).unwrap_or("").to_string())
            .collect();
        let mut rows = stmt.query_map(refs.as_slice(), |row| {
            Ok(rusqlite_row_to_dbrow(row, col_count, &col_names))
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    fn placeholder(&self, n: usize) -> String {
        format!("?{n}")
    }

    fn now_expr(&self) -> &'static str {
        "datetime('now')"
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        format!("MAX({a}, {b})")
    }

    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn table_exists(&self, name: &str) -> anyhow::Result<bool> {
        sqlite_table_exists(self, name)
    }

    fn get_table_columns(&self, table: &str) -> anyhow::Result<HashSet<String>> {
        sqlite_get_table_columns(self, table)
    }

    fn get_table_column_types(&self, table: &str) -> anyhow::Result<HashMap<String, String>> {
        sqlite_get_table_column_types(self, table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> anyhow::Result<Vec<String>> {
        sqlite_index_names(self, table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        "TEXT DEFAULT (datetime('now'))"
    }

    fn timestamp_column_type(&self) -> &'static str {
        "TEXT"
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        sqlite_column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        sqlite_date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        format!("json_extract({}, '$.{}')", column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        format!("json_each({}) AS {}", source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        sqlite_build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        sqlite_build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        true
    }

    fn like_operator(&self) -> &'static str {
        "LIKE"
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        sqlite_list_user_tables(self)
    }

    fn supports_drop_column(&self) -> bool {
        sqlite_supports_drop_column(self)
    }

    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()> {
        sqlite_vacuum_into(self, dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        SQLITE_SIDECAR_EXTENSIONS
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        sqlite_normalize_timestamp(ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;
    use crate::db::{BoxedConnection, pool};
    use tempfile::TempDir;

    fn temp_conn() -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let pool = pool::create_pool(dir.path(), &config).unwrap();
        let conn = pool.get().unwrap();
        (dir, conn)
    }

    #[test]
    fn execute_and_query() {
        let (_dir, conn) = temp_conn();
        conn.execute_batch("CREATE TABLE t (id TEXT, n INTEGER)")
            .unwrap();
        conn.execute(
            "INSERT INTO t (id, n) VALUES (?1, ?2)",
            &[DbValue::Text("a".into()), DbValue::Integer(42)],
        )
        .unwrap();

        let rows = conn.query_all("SELECT id, n FROM t", &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_string("id").unwrap(), "a");
        assert_eq!(rows[0].get_i64("n").unwrap(), 42);
    }

    #[test]
    fn query_one_returns_none_for_empty() {
        let (_dir, conn) = temp_conn();
        conn.execute_batch("CREATE TABLE t (id TEXT)").unwrap();
        let row = conn
            .query_one(
                "SELECT id FROM t WHERE id = ?1",
                &[DbValue::Text("x".into())],
            )
            .unwrap();
        assert!(row.is_none());
    }

    #[test]
    fn query_one_returns_first_row() {
        let (_dir, conn) = temp_conn();
        conn.execute_batch("CREATE TABLE t (id TEXT)").unwrap();
        conn.execute(
            "INSERT INTO t (id) VALUES (?1)",
            &[DbValue::Text("a".into())],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO t (id) VALUES (?1)",
            &[DbValue::Text("b".into())],
        )
        .unwrap();
        let row = conn.query_one("SELECT id FROM t ORDER BY id", &[]).unwrap();
        assert_eq!(row.unwrap().get_string("id").unwrap(), "a");
    }

    #[test]
    fn transaction_commit() {
        let (_dir, mut conn) = temp_conn();
        conn.execute_batch("CREATE TABLE t (id TEXT)").unwrap();

        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO t (id) VALUES (?1)",
            &[DbValue::Text("a".into())],
        )
        .unwrap();
        tx.commit().unwrap();

        let rows = conn.query_all("SELECT id FROM t", &[]).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn transaction_rollback_on_drop() {
        let (_dir, mut conn) = temp_conn();
        conn.execute_batch("CREATE TABLE t (id TEXT)").unwrap();

        {
            let tx = conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO t (id) VALUES (?1)",
                &[DbValue::Text("a".into())],
            )
            .unwrap();
            // drop without commit
        }

        let rows = conn.query_all("SELECT id FROM t", &[]).unwrap();
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn placeholder_format() {
        let (_dir, conn) = temp_conn();
        assert_eq!(conn.placeholder(1), "?1");
        assert_eq!(conn.placeholder(42), "?42");
    }

    #[test]
    fn now_expr_format() {
        let (_dir, conn) = temp_conn();
        assert_eq!(conn.now_expr(), "datetime('now')");
    }

    #[test]
    fn null_and_blob_values() {
        let (_dir, conn) = temp_conn();
        conn.execute_batch("CREATE TABLE t (id TEXT, data BLOB, empty TEXT)")
            .unwrap();
        conn.execute(
            "INSERT INTO t (id, data, empty) VALUES (?1, ?2, ?3)",
            &[
                DbValue::Text("a".into()),
                DbValue::Blob(vec![1, 2, 3]),
                DbValue::Null,
            ],
        )
        .unwrap();

        let rows = conn.query_all("SELECT * FROM t", &[]).unwrap();
        assert_eq!(
            rows[0].get_named("data"),
            Some(&DbValue::Blob(vec![1, 2, 3]))
        );
        assert_eq!(rows[0].get_named("empty"), Some(&DbValue::Null));
    }

    #[test]
    fn from_sqlite_value_converts() {
        assert_eq!(from_sqlite_value(&SqliteValue::Null), DbValue::Null);
        assert_eq!(
            from_sqlite_value(&SqliteValue::Integer(42)),
            DbValue::Integer(42)
        );
        assert_eq!(
            from_sqlite_value(&SqliteValue::Real(3.15)),
            DbValue::Real(3.15)
        );
        assert_eq!(
            from_sqlite_value(&SqliteValue::Text("hi".into())),
            DbValue::Text("hi".into())
        );
        assert_eq!(
            from_sqlite_value(&SqliteValue::Blob(vec![1])),
            DbValue::Blob(vec![1])
        );
    }

    #[test]
    fn transaction_immediate_works() {
        let (_dir, mut conn) = temp_conn();
        conn.execute_batch("CREATE TABLE t (id TEXT)").unwrap();

        let tx = conn.transaction_immediate().unwrap();
        tx.execute(
            "INSERT INTO t (id) VALUES (?1)",
            &[DbValue::Text("a".into())],
        )
        .unwrap();
        tx.commit().unwrap();

        let rows = conn.query_all("SELECT id FROM t", &[]).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn normalize_timestamp_sqlite_format() {
        assert_eq!(
            sqlite_normalize_timestamp("2024-01-15 12:30:45"),
            "2024-01-15T12:30:45.000Z"
        );
    }

    #[test]
    fn normalize_timestamp_already_iso() {
        let iso = "2024-01-15T12:30:45.000Z";
        assert_eq!(sqlite_normalize_timestamp(iso), iso);
    }

    // ── sqlite_date_offset_expr sign handling ──────────────────────────

    /// Regression: positive seconds must produce a negative offset modifier
    /// (subtracting time), and negative seconds must produce a positive
    /// offset modifier (adding time).
    #[test]
    fn date_offset_expr_positive_input() {
        let (_expr, value) = sqlite_date_offset_expr(30, 1);
        assert_eq!(
            value,
            DbValue::Text("-30 seconds".to_string()),
            "positive input should produce negative offset"
        );
    }

    #[test]
    fn date_offset_expr_negative_input() {
        let (_expr, value) = sqlite_date_offset_expr(-30, 1);
        assert_eq!(
            value,
            DbValue::Text("+30 seconds".to_string()),
            "negative input should produce positive offset"
        );
    }

    #[test]
    fn date_offset_expr_zero() {
        let (_expr, value) = sqlite_date_offset_expr(0, 1);
        assert_eq!(
            value,
            DbValue::Text("-0 seconds".to_string()),
            "zero should produce -0 seconds"
        );
    }

    #[test]
    fn date_offset_expr_sql_format() {
        let (expr, _value) = sqlite_date_offset_expr(30, 3);
        assert_eq!(
            expr, "datetime('now', ?3)",
            "SQL expression should use the given param position"
        );
    }

    /// Regression: multi-byte UTF-8 input must not panic from string slicing.
    #[test]
    fn normalize_timestamp_multibyte_utf8_no_panic() {
        // 19-byte string with multi-byte chars -- should pass through unchanged
        let input = "日本語テスト入力値";
        assert_eq!(sqlite_normalize_timestamp(input), input);
    }
}
