//! Database connection trait — object-safe abstraction over backend-specific connections.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::core::FieldType;

use super::types::{DbRow, DbValue};

/// Object-safe database connection trait.
///
/// All query functions accept `&dyn DbConnection`, making them backend-agnostic.
/// The SQLite implementation lives in `sqlite.rs`.
pub trait DbConnection {
    /// Execute a statement that modifies data. Returns the number of rows affected.
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize>;

    /// Execute multiple statements as a batch (no parameters).
    fn execute_batch(&self, sql: &str) -> Result<()>;

    /// Execute a query and return all matching rows.
    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>>;

    /// Execute a query and return the first row, or `None` if no rows match.
    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>>;

    /// Return the placeholder syntax for parameter `n` (1-based).
    ///
    /// SQLite: `"?1"`, `"?2"`, ...
    /// PostgreSQL: `"$1"`, `"$2"`, ...
    fn placeholder(&self, n: usize) -> String;

    /// Return the SQL expression for the current timestamp.
    ///
    /// SQLite: `"datetime('now')"`
    /// PostgreSQL: `"NOW()"`
    fn now_expr(&self) -> &'static str;

    /// Return a SQL expression for `max(a, b)` as a scalar (not aggregate).
    ///
    /// SQLite: `"MAX(a, b)"` (SQLite's `MAX` with 2+ args is scalar)
    /// PostgreSQL: `"GREATEST(a, b)"`
    fn greatest_expr(&self, a: &str, b: &str) -> String;

    /// Return the backend identifier.
    ///
    /// Used to gate backend-specific features (FTS5, `sqlite_master`,
    /// `json_extract`, etc.) that have no cross-backend abstraction.
    fn kind(&self) -> &'static str;

    // ── Schema introspection ─────────────────────────────────────────

    /// Check whether a table exists in the database.
    fn table_exists(&self, name: &str) -> Result<bool>;

    /// Get the set of column names for a table.
    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>>;

    /// Get a mapping of column name to column type for a table.
    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>>;

    /// Get index names for a table matching a name prefix.
    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>>;

    // ── DDL helpers ──────────────────────────────────────────────────

    /// DDL fragment for a timestamp column with `DEFAULT = now()`.
    ///
    /// SQLite: `"TEXT DEFAULT (datetime('now'))"`
    /// Postgres: `"TIMESTAMPTZ DEFAULT NOW()"`
    fn timestamp_column_default(&self) -> &'static str;

    /// DDL type for a nullable timestamp column (no default).
    ///
    /// SQLite: `"TEXT"` · Postgres: `"TIMESTAMPTZ"`
    fn timestamp_column_type(&self) -> &'static str;

    /// SQL column type for a field type.
    fn column_type_for(&self, ft: &FieldType) -> &'static str;

    // ── DML helpers ──────────────────────────────────────────────────

    /// SQL expression for `now() - N seconds` and the parameter value to bind.
    /// Backend controls both SQL syntax and parameter format.
    ///
    /// SQLite: `("datetime('now', ?N)", Text("-30 seconds"))`
    /// Postgres: `("NOW() + $N::interval", Text("-30 seconds"))`
    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue);

    // ── JSON functions ───────────────────────────────────────────────

    /// SQL expression for extracting a JSON field from a column.
    ///
    /// `column`: the SQL expression (e.g. `"data"`, `"j0.value"`).
    /// `field`: the field name without path prefix (e.g. `"body"`).
    ///
    /// SQLite: `"json_extract(data, '$.body')"`
    fn json_extract_expr(&self, column: &str, field: &str) -> String;

    /// FROM-clause fragment for iterating a JSON array.
    ///
    /// SQLite: `"json_each(source) AS alias"`
    fn json_each_source(&self, source: &str, alias: &str) -> String;

    // ── Conflict handling ────────────────────────────────────────────

    /// Build a complete INSERT-or-skip SQL statement.
    ///
    /// SQLite: `INSERT OR IGNORE INTO {table} ({columns}) VALUES ({values})`
    /// Postgres: `INSERT INTO {table} ({columns}) VALUES ({values}) ON CONFLICT DO NOTHING`
    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String;

    /// Build a complete upsert SQL statement.
    /// `columns` are raw names — the backend quotes them as needed.
    /// `key_col` is the conflict target (usually `"id"`).
    ///
    /// SQLite: `INSERT OR REPLACE INTO {table} ("c1","c2") VALUES ({values})`
    /// Postgres: `INSERT INTO {table} ("c1","c2") VALUES ({values})
    ///           ON CONFLICT ("id") DO UPDATE SET "c1"=EXCLUDED."c1", ...`
    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String;

    // ── Capability flags ─────────────────────────────────────────────

    /// Whether this backend supports FTS5 full-text search.
    fn supports_fts(&self) -> bool;

    /// Case-insensitive LIKE operator.
    ///
    /// SQLite/MySQL: `"LIKE"` · Postgres: `"ILIKE"`
    fn like_operator(&self) -> &'static str;

    /// List all user-created table names (excludes system/internal tables).
    ///
    /// SQLite: queries `sqlite_master` · Postgres: `information_schema.tables`
    fn list_user_tables(&self) -> Result<Vec<String>>;

    /// Whether `ALTER TABLE ... DROP COLUMN` is supported.
    ///
    /// SQLite: `true` for version ≥ 3.35.0 · Postgres/MySQL: always `true`
    fn supports_drop_column(&self) -> bool;

    /// Create a consistent backup snapshot of the database at `dest`.
    ///
    /// SQLite: `VACUUM INTO <dest>` · Postgres: `pg_dump` or equivalent
    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()>;

    /// File extensions for sidecar files that should be cleaned up on restore.
    ///
    /// SQLite: `["db-wal", "db-shm"]` · Postgres: `[]`
    fn sidecar_extensions(&self) -> &[&str];

    /// Normalize a timestamp from the backend's native format to ISO 8601.
    /// Already-normalized values pass through unchanged.
    ///
    /// SQLite: `"2024-01-01 12:00:00"` → `"2024-01-01T12:00:00.000Z"`
    fn normalize_timestamp(&self, ts: &str) -> String;
}

/// Private trait for backend connection implementations.
///
/// Each backend (SQLite, PostgreSQL, ...) implements this on its connection
/// type. Callers never see this — they interact through `BoxedConnection`.
pub(crate) trait ConnectionInner: DbConnection + Send {
    /// Open a deferred transaction and return it boxed.
    fn transaction_boxed(&mut self) -> Result<Box<dyn TransactionInner + '_>>;

    /// Open an IMMEDIATE transaction and return it boxed.
    fn transaction_immediate_boxed(&mut self) -> Result<Box<dyn TransactionInner + '_>>;
}

/// Private trait for backend transaction implementations.
///
/// Each backend implements this on its transaction type.
/// Callers never see this — they interact through `BoxedTransaction`.
pub(crate) trait TransactionInner: DbConnection {
    /// Commit this transaction (consumes the boxed self).
    fn commit_inner(self: Box<Self>) -> Result<()>;
}

/// Backend-agnostic database connection.
///
/// Wraps a boxed `ConnectionInner` so callers never see concrete backend types.
/// Obtained from `DbPool::get()`. Implements `DbConnection` for read queries
/// and provides `transaction()` / `transaction_immediate()` for write operations.
pub struct BoxedConnection {
    inner: Box<dyn ConnectionInner>,
}

impl BoxedConnection {
    /// Wrap a backend connection.
    pub(crate) fn new(inner: Box<dyn ConnectionInner>) -> Self {
        Self { inner }
    }

    /// Open a deferred transaction.
    pub fn transaction(&mut self) -> Result<BoxedTransaction<'_>> {
        let tx = self.inner.transaction_boxed()?;
        Ok(BoxedTransaction { inner: tx })
    }

    /// Open an IMMEDIATE transaction (write-lock from the start).
    pub fn transaction_immediate(&mut self) -> Result<BoxedTransaction<'_>> {
        let tx = self.inner.transaction_immediate_boxed()?;
        Ok(BoxedTransaction { inner: tx })
    }
}

impl DbConnection for BoxedConnection {
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        self.inner.execute(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<()> {
        self.inner.execute_batch(sql)
    }

    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
        self.inner.query_all(sql, params)
    }

    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
        self.inner.query_one(sql, params)
    }

    fn placeholder(&self, n: usize) -> String {
        self.inner.placeholder(n)
    }

    fn now_expr(&self) -> &'static str {
        self.inner.now_expr()
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        self.inner.greatest_expr(a, b)
    }

    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        self.inner.table_exists(name)
    }

    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
        self.inner.get_table_columns(table)
    }

    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
        self.inner.get_table_column_types(table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
        self.inner.index_names(table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        self.inner.timestamp_column_default()
    }

    fn timestamp_column_type(&self) -> &'static str {
        self.inner.timestamp_column_type()
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        self.inner.column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        self.inner.date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        self.inner.json_extract_expr(column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        self.inner.json_each_source(source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        self.inner.build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        self.inner.build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        self.inner.supports_fts()
    }

    fn like_operator(&self) -> &'static str {
        self.inner.like_operator()
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        self.inner.list_user_tables()
    }

    fn supports_drop_column(&self) -> bool {
        self.inner.supports_drop_column()
    }

    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()> {
        self.inner.vacuum_into(dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        self.inner.sidecar_extensions()
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        self.inner.normalize_timestamp(ts)
    }
}

/// Backend-agnostic database transaction.
///
/// Wraps a boxed `TransactionInner`. Implements `DbConnection` so it can be
/// passed to any query function. Call `commit()` to finalize; dropping without
/// commit rolls back.
pub struct BoxedTransaction<'conn> {
    inner: Box<dyn TransactionInner + 'conn>,
}

impl BoxedTransaction<'_> {
    /// Commit this transaction.
    pub fn commit(self) -> Result<()> {
        self.inner.commit_inner()
    }
}

impl DbConnection for BoxedTransaction<'_> {
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        self.inner.execute(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<()> {
        self.inner.execute_batch(sql)
    }

    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
        self.inner.query_all(sql, params)
    }

    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
        self.inner.query_one(sql, params)
    }

    fn placeholder(&self, n: usize) -> String {
        self.inner.placeholder(n)
    }

    fn now_expr(&self) -> &'static str {
        self.inner.now_expr()
    }

    fn greatest_expr(&self, a: &str, b: &str) -> String {
        self.inner.greatest_expr(a, b)
    }

    fn kind(&self) -> &'static str {
        self.inner.kind()
    }

    fn table_exists(&self, name: &str) -> Result<bool> {
        self.inner.table_exists(name)
    }

    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
        self.inner.get_table_columns(table)
    }

    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
        self.inner.get_table_column_types(table)
    }

    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
        self.inner.index_names(table, prefix)
    }

    fn timestamp_column_default(&self) -> &'static str {
        self.inner.timestamp_column_default()
    }

    fn timestamp_column_type(&self) -> &'static str {
        self.inner.timestamp_column_type()
    }

    fn column_type_for(&self, ft: &FieldType) -> &'static str {
        self.inner.column_type_for(ft)
    }

    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
        self.inner.date_offset_expr(seconds, param_pos)
    }

    fn json_extract_expr(&self, column: &str, field: &str) -> String {
        self.inner.json_extract_expr(column, field)
    }

    fn json_each_source(&self, source: &str, alias: &str) -> String {
        self.inner.json_each_source(source, alias)
    }

    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
        self.inner.build_insert_ignore(table, columns, values)
    }

    fn build_upsert(&self, table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
        self.inner.build_upsert(table, columns, values, key_col)
    }

    fn supports_fts(&self) -> bool {
        self.inner.supports_fts()
    }

    fn like_operator(&self) -> &'static str {
        self.inner.like_operator()
    }

    fn list_user_tables(&self) -> Result<Vec<String>> {
        self.inner.list_user_tables()
    }

    fn supports_drop_column(&self) -> bool {
        self.inner.supports_drop_column()
    }

    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()> {
        self.inner.vacuum_into(dest)
    }

    fn sidecar_extensions(&self) -> &[&str] {
        self.inner.sidecar_extensions()
    }

    fn normalize_timestamp(&self, ts: &str) -> String {
        self.inner.normalize_timestamp(ts)
    }
}
