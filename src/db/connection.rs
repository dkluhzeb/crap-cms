//! Database connection trait — object-safe abstraction over backend-specific connections.

use std::{
    collections::{HashMap, HashSet},
    result::Result as StdResult,
};

use anyhow::{Context as _, Result};
use tracing::error;

use crate::core::FieldType;

use super::types::{DbRow, DbValue};

/// The shape of an upsert: the row to write, the column it conflicts on, and
/// the condition an existing row must satisfy for the update half to apply.
///
/// The guard is what makes a *claim* atomic. Spelled as a pair of statements —
/// insert if absent, else update if stale — two workers racing the very first
/// claim both find the row absent, and the loser's INSERT fails on the primary
/// key: a failed tick rather than "someone else won". An IMMEDIATE transaction
/// does not save it either, since on Postgres that is a plain `BEGIN` at READ
/// COMMITTED. One guarded upsert has no such window on either backend, and the
/// caller reads the outcome off the affected-row count.
#[derive(Debug, Clone, Copy)]
pub struct UpsertSpec<'a> {
    pub(crate) table: &'a str,
    pub(crate) columns: &'a [&'a str],
    pub(crate) values: &'a str,
    pub(crate) key_col: &'a str,
    pub(crate) guard: Option<&'a str>,
}

impl<'a> UpsertSpec<'a> {
    /// Start building an upsert into `table` conflicting on `key_col`.
    #[must_use]
    pub fn builder(table: &'a str, key_col: &'a str) -> UpsertSpecBuilder<'a> {
        UpsertSpecBuilder {
            spec: Self {
                table,
                columns: &[],
                values: "",
                key_col,
                guard: None,
            },
        }
    }
}

/// Builder for [`UpsertSpec`].
pub struct UpsertSpecBuilder<'a> {
    spec: UpsertSpec<'a>,
}

impl<'a> UpsertSpecBuilder<'a> {
    /// The columns written and the placeholder list binding them, in the same
    /// order. `columns` are raw names — the backend quotes them.
    #[must_use]
    pub fn columns(mut self, columns: &'a [&'a str], values: &'a str) -> Self {
        self.spec.columns = columns;
        self.spec.values = values;
        self
    }

    /// A predicate over the row **already stored**, qualified with the table
    /// name. The `DO UPDATE` applies only where it holds; everywhere else the
    /// statement affects zero rows and changes nothing.
    #[must_use]
    pub fn guard(mut self, guard: &'a str) -> Self {
        self.spec.guard = Some(guard);
        self
    }

    #[must_use]
    pub fn build(self) -> UpsertSpec<'a> {
        self.spec
    }
}

/// Object-safe database connection trait.
///
/// All query functions accept `&dyn DbConnection`, making them backend-agnostic.
/// The `SQLite` implementation lives in `sqlite.rs`.
pub trait DbConnection {
    /// Execute a statement that modifies data. Returns the number of rows affected.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the SQL is invalid, parameters fail to bind,
    /// or the database rejects the statement.
    fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize>;

    /// Execute multiple statements as a batch (no parameters).
    ///
    /// # Errors
    ///
    /// Returns a backend error if any statement in the batch fails.
    fn execute_batch(&self, sql: &str) -> Result<()>;

    /// Lock a single row for the rest of the transaction so concurrent writers
    /// to the same row serialize. Postgres issues `SELECT … FOR UPDATE`;
    /// `SQLite`'s IMMEDIATE transaction already holds the write lock (writers
    /// are serialized), so the default is a no-op. Used before an unlocked
    /// read-then-write (e.g. the ref-count snapshot) to close a TOCTOU on the
    /// document row under Postgres MVCC.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the lock query fails.
    fn lock_row(&self, table: &str, id: &str) -> Result<()> {
        let _ = (table, id);
        Ok(())
    }

    /// Acquire a transaction-scoped advisory lock keyed by `key`, serializing a
    /// critical section across connections/nodes until the current transaction
    /// commits or rolls back. Default: no-op — `SQLite`'s `IMMEDIATE`
    /// transaction already serializes all writers, so a single-writer backend
    /// needs no advisory lock.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the lock query fails.
    fn advisory_xact_lock(&self, key: i64) -> Result<()> {
        let _ = key;
        Ok(())
    }

    /// Execute a DDL statement (CREATE TABLE, ALTER TABLE, etc.).
    /// On Postgres, automatically adjusts `INTEGER` to `BIGINT` since
    /// `DbValue::Integer` is `i64` which tokio-postgres binds to `int8`.
    /// Default: delegates to `execute`.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the DDL is invalid or rejected.
    fn execute_ddl(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
        self.execute(sql, params)
    }

    /// Execute a batch DDL statement (no parameters).
    /// Same adjustment as `execute_ddl`.
    /// Default: delegates to `execute_batch`.
    ///
    /// # Errors
    ///
    /// Returns a backend error if any DDL statement in the batch fails.
    fn execute_batch_ddl(&self, sql: &str) -> Result<()> {
        self.execute_batch(sql)
    }

    /// Execute a query and return all matching rows.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the query is invalid or execution fails.
    fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>>;

    /// Execute a query and return the first row, or `None` if no rows match.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the query is invalid or execution fails.
    fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>>;

    /// Return the placeholder syntax for parameter `n` (1-based).
    ///
    /// `SQLite`: `"?1"`, `"?2"`, ...
    /// `PostgreSQL`: `"$1"`, `"$2"`, ...
    fn placeholder(&self, n: usize) -> String;

    /// Return the SQL expression for the current timestamp, in the ISO-8601
    /// `…Z` format shared with `utc_now()` on both backends (see the
    /// timestamp-format frozen contract).
    ///
    /// `SQLite`: `strftime('%Y-%m-%dT%H:%M:%fZ', 'now')`
    /// `PostgreSQL`: `to_char(NOW(), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')`
    fn now_expr(&self) -> &'static str;

    /// Return a SQL expression for `max(a, b)` as a scalar (not aggregate).
    ///
    /// `SQLite`: `"MAX(a, b)"` (`SQLite`'s `MAX` with 2+ args is scalar)
    /// `PostgreSQL`: `"GREATEST(a, b)"`
    fn greatest_expr(&self, a: &str, b: &str) -> String;

    /// Return the backend identifier.
    ///
    /// Used to gate backend-specific features (FTS5, `sqlite_master`,
    /// `json_extract`, etc.) that have no cross-backend abstraction. Prefer the
    /// [`is_postgres`](Self::is_postgres) / [`is_sqlite`](Self::is_sqlite)
    /// predicates over comparing this string at call sites.
    fn kind(&self) -> &'static str;

    /// Whether this connection targets `PostgreSQL`. One typo-proof classifier so
    /// backend gates don't hand-compare `kind() == "postgres"`.
    fn is_postgres(&self) -> bool {
        self.kind() == "postgres"
    }

    /// Whether this connection targets `SQLite`. Companion to [`is_postgres`](Self::is_postgres).
    fn is_sqlite(&self) -> bool {
        self.kind() == "sqlite"
    }

    // ── Schema introspection ─────────────────────────────────────────

    /// Check whether a table exists in the database.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the schema-introspection query fails.
    fn table_exists(&self, name: &str) -> Result<bool>;

    /// Get the set of column names for a table.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the schema-introspection query fails.
    fn get_table_columns(&self, table: &str) -> Result<HashSet<String>>;

    /// Get a mapping of column name to column type for a table.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the schema-introspection query fails.
    fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>>;

    /// Get index names for a table matching a name prefix.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the schema-introspection query fails.
    fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>>;

    // ── DDL helpers ──────────────────────────────────────────────────

    /// DDL fragment for a timestamp column with `DEFAULT = now()`.
    ///
    /// `SQLite`: `"TEXT DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))"`
    /// Postgres: `"TEXT DEFAULT to_char(NOW(), 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')"`
    /// Both store ISO 8601 text, so timestamps compare as strings.
    fn timestamp_column_default(&self) -> &'static str;

    /// DDL type for a nullable timestamp column (no default).
    ///
    /// `TEXT` on both backends (see [`timestamp_column_default`](Self::timestamp_column_default)).
    fn timestamp_column_type(&self) -> &'static str;

    /// SQL column type for a field type.
    fn column_type_for(&self, ft: &FieldType) -> &'static str;

    // ── DML helpers ──────────────────────────────────────────────────

    /// SQL expression for `now() - seconds` and the parameter value to bind.
    /// A positive `seconds` yields a timestamp in the **past**; a negative
    /// `seconds` yields one in the **future**. Both backends must agree on
    /// this `now - seconds` contract. Backend controls SQL syntax and param
    /// format.
    ///
    /// `SQLite`: `("datetime('now', ?N)", Text("-30 seconds"))` for `seconds = 30`.
    /// Postgres: `("to_char(NOW() - make_interval(secs => $N), …)", Real(30.0))`.
    fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue);

    // ── JSON functions ───────────────────────────────────────────────

    /// SQL expression for extracting a JSON field from a column.
    ///
    /// `column`: the SQL expression (e.g. `"data"`, `"j0.value"`).
    /// `field`: the field name without path prefix (e.g. `"body"`).
    ///
    /// `SQLite`: `"json_extract(data, '$.body')"`
    fn json_extract_expr(&self, column: &str, field: &str) -> String;

    /// Wrap a JSON-extract expression so a `Number` sub-field compares
    /// numerically. `SQLite`'s `json_extract` already yields the native numeric
    /// type (default identity), but Postgres `#>>`/`->>` yield `text`, so a
    /// `text <op> float8` comparison would error or compare lexically — PG
    /// overrides this to add a numeric cast.
    fn json_number_cast(&self, expr: &str) -> String {
        expr.to_string()
    }

    /// Wrap a JSON-extract expression so a `Checkbox` sub-field — stored as
    /// JSON `true`/`false` — compares as the integer its operand binds as.
    /// `SQLite`'s `json_extract` already yields `1`/`0` (default identity);
    /// Postgres `#>>` yields the text `'true'`/`'false'`, which PG overrides to
    /// map to `1`/`0` (as it does a stored `1`/`0`), so the comparison is not a
    /// `text = bigint` error.
    fn json_checkbox_cast(&self, expr: &str) -> String {
        expr.to_string()
    }

    /// FROM-clause fragment for iterating a JSON array.
    ///
    /// `SQLite`: `"json_each(source) AS alias"`
    fn json_each_source(&self, source: &str, alias: &str) -> String;

    /// The part of the text `expr` after the first `separator` — the whole
    /// text when it holds none. `separator` is a literal the caller controls.
    ///
    /// `SQLite`: `substr` + `instr` · Postgres: `substr` + `strpos`
    fn text_after(&self, expr: &str, separator: &str) -> String;

    // ── Conflict handling ────────────────────────────────────────────

    /// Build a complete INSERT-or-skip SQL statement.
    ///
    /// `SQLite`: `INSERT OR IGNORE INTO {table} ({columns}) VALUES ({values})`
    /// Postgres: `INSERT INTO {table} ({columns}) VALUES ({values}) ON CONFLICT DO NOTHING`
    fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String;

    /// Build a complete upsert SQL statement from [`UpsertSpec`].
    ///
    /// `INSERT INTO {table} ("c1","c2") VALUES ({values})
    ///  ON CONFLICT ("id") DO UPDATE SET "c1" = excluded."c1", …[ WHERE guard]`
    fn build_upsert(&self, spec: &UpsertSpec<'_>) -> String;

    // ── Capability flags ─────────────────────────────────────────────

    /// Whether this backend supports FTS5 full-text search.
    fn supports_fts(&self) -> bool;

    /// Case-insensitive LIKE operator.
    ///
    /// SQLite/MySQL: `"LIKE"` · Postgres: `"ILIKE"`
    fn like_operator(&self) -> &'static str;

    /// List all user-created table names (excludes system/internal tables).
    ///
    /// `SQLite`: queries `sqlite_master` · Postgres: `information_schema.tables`
    ///
    /// # Errors
    ///
    /// Returns a backend error if the schema-introspection query fails.
    fn list_user_tables(&self) -> Result<Vec<String>>;

    /// Whether `ALTER TABLE ... DROP COLUMN` is supported.
    ///
    /// `SQLite`: `true` for version ≥ 3.35.0 · Postgres/MySQL: always `true`
    fn supports_drop_column(&self) -> bool;

    /// Create a consistent backup snapshot of the database at `dest`.
    ///
    /// `SQLite`: `VACUUM INTO <dest>` · Postgres: `pg_dump` or equivalent
    ///
    /// # Errors
    ///
    /// Returns a backend error if the backup operation fails or the
    /// destination path is not writable.
    fn vacuum_into(&self, dest: &std::path::Path) -> Result<()>;

    /// File extensions for sidecar files that should be cleaned up on restore.
    ///
    /// `SQLite`: `["db-wal", "db-shm"]` · Postgres: `[]`
    fn sidecar_extensions(&self) -> &[&str];

    /// Normalize a timestamp from the backend's native format to ISO 8601.
    /// Already-normalized values pass through unchanged.
    ///
    /// `SQLite`: `"2024-01-01 12:00:00"` → `"2024-01-01T12:00:00.000Z"`
    fn normalize_timestamp(&self, ts: &str) -> String;

    // ── Transactions opened in place ─────────────────────────────────

    /// Whether a transaction is open on this connection — one it is, or one
    /// [`begin_in_place`](Self::begin_in_place) opened on it.
    fn in_transaction(&self) -> bool;

    /// Open a write transaction on this connection itself, for a scope that
    /// cannot hold a [`BoxedTransaction`] borrowing its connection (see
    /// [`InPlaceTransaction`]).
    ///
    /// `SQLite`: `BEGIN IMMEDIATE` — the write lock up front, exactly like
    /// [`BoxedConnection::transaction_immediate`], so a read followed by a
    /// write cannot fail with `SQLITE_BUSY_SNAPSHOT`. Postgres: `BEGIN`.
    ///
    /// # Errors
    ///
    /// Returns a backend error if a transaction is already open or the
    /// database refuses to start one (on `SQLite`: the write lock was not
    /// granted within the busy timeout).
    fn begin_in_place(&self) -> Result<()>;

    /// Commit the transaction [`begin_in_place`](Self::begin_in_place)
    /// opened.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the commit fails — including when the
    /// database rolled the transaction back instead of committing it
    /// (Postgres answers `COMMIT` on a transaction a failed statement aborted
    /// with a `ROLLBACK`, which the driver would otherwise report as success).
    fn commit_in_place(&self) -> Result<()>;

    /// Roll back the transaction [`begin_in_place`](Self::begin_in_place)
    /// opened.
    ///
    /// # Errors
    ///
    /// Returns a backend error if the rollback fails.
    fn rollback_in_place(&self) -> Result<()>;
}

/// The savepoint [`with_savepoint`] opens. Savepoints of one name nest: each
/// `RELEASE` / `ROLLBACK TO` addresses the most recent one, so a step inside
/// a step needs no name of its own.
const STEP_SAVEPOINT: &str = "SAVEPOINT crap_step";
const STEP_RELEASE: &str = "RELEASE SAVEPOINT crap_step";
const STEP_ROLLBACK: &str = "ROLLBACK TO SAVEPOINT crap_step; RELEASE SAVEPOINT crap_step";

/// Run `work` as one atomic step of the transaction open on `conn`: inside a
/// savepoint that is released when `work` succeeds and rolled back to when it
/// fails. A failed step therefore leaves no partial writes behind, and the
/// transaction stays usable after it on both backends — on Postgres a failed
/// statement otherwise aborts the whole transaction, and its `COMMIT` then
/// silently rolls back every earlier write as well.
///
/// With no transaction open on `conn`, `work` runs as is: every statement
/// commits on its own and there is nothing to step back to.
///
/// A step that reports success while its transaction is unusable (it caught a
/// failed statement's error itself, on Postgres) is rolled back to its start
/// and reported as failed.
///
/// # Errors
///
/// The outer error is the savepoint's own: it could not be opened, released
/// or rolled back to. The inner result is `work`'s.
pub fn with_savepoint<T, E>(
    conn: &dyn DbConnection,
    work: impl FnOnce() -> StdResult<T, E>,
) -> Result<StdResult<T, E>> {
    if !conn.in_transaction() {
        return Ok(work());
    }

    conn.execute_batch(STEP_SAVEPOINT)
        .context("failed to open a savepoint for the step")?;

    let result = work();

    if result.is_ok() {
        let Err(e) = conn.execute_batch(STEP_RELEASE) else {
            return Ok(result);
        };

        conn.execute_batch(STEP_ROLLBACK)
            .context("failed to roll back to the step's savepoint")?;

        return Err(e.context("the step left its transaction unusable and was rolled back"));
    }

    conn.execute_batch(STEP_ROLLBACK)
        .context("failed to roll back to the step's savepoint")?;

    Ok(result)
}

/// The connection an [`InPlaceTransaction`] runs on.
enum Held<'c> {
    /// A connection the transaction owns — it goes back to its pool when the
    /// transaction is settled.
    Owned(BoxedConnection),
    /// A connection the caller owns and lends for the transaction's lifetime.
    Borrowed(&'c dyn DbConnection),
}

impl Held<'_> {
    fn conn(&self) -> &dyn DbConnection {
        match self {
            Self::Owned(conn) => conn,
            Self::Borrowed(conn) => *conn,
        }
    }
}

/// A write transaction opened on a connection in place
/// ([`DbConnection::begin_in_place`]): IMMEDIATE on `SQLite`, and committed
/// through [`DbConnection::commit_in_place`], which refuses to report success
/// for a transaction the database rolled back.
///
/// Unlike [`BoxedTransaction`] it does not hold its connection mutably
/// borrowed, so a scope can open it lazily, or run it on a connection it was
/// only lent. Dropping it without committing rolls it back.
pub struct InPlaceTransaction<'c> {
    held: Option<Held<'c>>,
}

impl InPlaceTransaction<'static> {
    /// Open a transaction on `conn`, which the transaction keeps until it is
    /// settled.
    ///
    /// # Errors
    ///
    /// Returns the backend error of [`DbConnection::begin_in_place`].
    pub fn begin_owned(conn: BoxedConnection) -> Result<Self> {
        Self::begin(Held::Owned(conn))
    }
}

impl<'c> InPlaceTransaction<'c> {
    /// Open a transaction on the lent connection `conn`.
    ///
    /// # Errors
    ///
    /// Returns the backend error of [`DbConnection::begin_in_place`].
    pub fn begin_on(conn: &'c dyn DbConnection) -> Result<Self> {
        Self::begin(Held::Borrowed(conn))
    }

    fn begin(held: Held<'c>) -> Result<Self> {
        held.conn().begin_in_place()?;

        Ok(Self { held: Some(held) })
    }

    /// The connection the transaction runs on.
    ///
    /// # Panics
    ///
    /// Never: the connection is only taken by `commit`, which consumes the
    /// transaction.
    #[must_use]
    pub fn conn(&self) -> &dyn DbConnection {
        self.held
            .as_ref()
            .map(Held::conn)
            .expect("an unsettled transaction holds its connection")
    }

    /// Commit the transaction; an owned connection goes back to its pool.
    ///
    /// # Errors
    ///
    /// Returns the commit error — the transaction is rolled back, so the
    /// connection never returns to its pool mid-transaction.
    pub fn commit(mut self) -> Result<()> {
        let Some(held) = self.held.take() else {
            return Ok(());
        };

        let conn = held.conn();
        let Err(e) = conn.commit_in_place() else {
            return Ok(());
        };

        roll_back(conn);

        Err(e)
    }
}

impl Drop for InPlaceTransaction<'_> {
    fn drop(&mut self) {
        if let Some(held) = self.held.take() {
            roll_back(held.conn());
        }
    }
}

/// Roll back `conn`'s in-place transaction, if one is still open; a failure
/// is logged — there is nothing left to undo it with.
fn roll_back(conn: &dyn DbConnection) {
    if !conn.in_transaction() {
        return;
    }

    let _ = conn
        .rollback_in_place()
        .inspect_err(|e| error!("transaction rollback failed: {e:#}"));
}

/// Private trait for backend connection implementations.
///
/// Each backend (`SQLite`, `PostgreSQL`, ...) implements this on its connection
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
    ///
    /// # Errors
    ///
    /// Returns a backend error if the transaction cannot be started (e.g.
    /// pool exhausted, connection lost).
    pub fn transaction(&mut self) -> Result<BoxedTransaction<'_>> {
        let tx = self.inner.transaction_boxed()?;
        Ok(BoxedTransaction { inner: tx })
    }

    /// Open an IMMEDIATE transaction (write-lock from the start).
    ///
    /// # Errors
    ///
    /// Returns a backend error if the transaction cannot be started or the
    /// write lock cannot be acquired.
    pub fn transaction_immediate(&mut self) -> Result<BoxedTransaction<'_>> {
        let tx = self.inner.transaction_immediate_boxed()?;
        Ok(BoxedTransaction { inner: tx })
    }
}

/// Implement `DbConnection` by delegating every method to `self.inner`.
macro_rules! impl_db_connection_delegate {
    ($ty:ty) => {
        impl DbConnection for $ty {
            fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
                self.inner.execute(sql, params)
            }
            fn execute_batch(&self, sql: &str) -> Result<()> {
                self.inner.execute_batch(sql)
            }
            fn execute_ddl(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
                self.inner.execute_ddl(sql, params)
            }
            fn execute_batch_ddl(&self, sql: &str) -> Result<()> {
                self.inner.execute_batch_ddl(sql)
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
            fn json_number_cast(&self, expr: &str) -> String {
                self.inner.json_number_cast(expr)
            }
            fn json_checkbox_cast(&self, expr: &str) -> String {
                self.inner.json_checkbox_cast(expr)
            }
            fn lock_row(&self, table: &str, id: &str) -> Result<()> {
                self.inner.lock_row(table, id)
            }
            fn advisory_xact_lock(&self, key: i64) -> Result<()> {
                self.inner.advisory_xact_lock(key)
            }
            fn json_each_source(&self, source: &str, alias: &str) -> String {
                self.inner.json_each_source(source, alias)
            }
            fn text_after(&self, expr: &str, separator: &str) -> String {
                self.inner.text_after(expr, separator)
            }
            fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
                self.inner.build_insert_ignore(table, columns, values)
            }
            fn build_upsert(&self, spec: &UpsertSpec<'_>) -> String {
                self.inner.build_upsert(spec)
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
            fn in_transaction(&self) -> bool {
                self.inner.in_transaction()
            }
            fn begin_in_place(&self) -> Result<()> {
                self.inner.begin_in_place()
            }
            fn commit_in_place(&self) -> Result<()> {
                self.inner.commit_in_place()
            }
            fn rollback_in_place(&self) -> Result<()> {
                self.inner.rollback_in_place()
            }
        }
    };
}

impl_db_connection_delegate!(BoxedConnection);

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
    ///
    /// # Errors
    ///
    /// Returns a backend error if the commit fails (e.g. constraint
    /// violation surfaced at commit time, lost connection).
    pub fn commit(self) -> Result<()> {
        self.inner.commit_inner()
    }
}

impl_db_connection_delegate!(BoxedTransaction<'_>);

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::time::Instant;

    use anyhow::{Error, anyhow};
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::CrapConfig,
        db::{DbPool, StatementDeadlineScope, StatementTimedOut, pool},
    };

    /// A file-backed pool with a short busy timeout and a `t` table.
    fn test_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("tmpdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        config.database.busy_timeout = 50;

        let pool = pool::create_pool(dir.path(), &config).expect("pool");
        pool.write()
            .expect("conn")
            .execute_batch("CREATE TABLE t (x INTEGER)")
            .expect("table");

        (dir, pool)
    }

    fn values(pool: &DbPool) -> Vec<i64> {
        pool.get()
            .expect("conn")
            .query_all("SELECT x FROM t ORDER BY x", &[])
            .expect("select")
            .iter()
            .filter_map(|r| r.i64_at(0))
            .collect()
    }

    fn insert(conn: &dyn DbConnection, x: i64) -> Result<usize> {
        conn.execute("INSERT INTO t (x) VALUES (?1)", &[DbValue::Integer(x)])
    }

    /// A failed step leaves none of its own writes behind, and the writes
    /// before and after it commit — the transaction is still usable.
    #[test]
    fn a_failed_step_is_rolled_back_to_its_start() {
        let (_dir, pool) = test_pool();
        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");
        let conn = tx.conn();

        insert(conn, 1).expect("before");

        let step = with_savepoint(conn, || -> Result<()> {
            insert(conn, 2)?;
            Err(anyhow!("the step fails after writing"))
        })
        .expect("the savepoint itself works");
        assert!(step.is_err(), "the step's own error is returned");

        insert(conn, 3).expect("after");
        tx.commit().expect("commit");

        assert_eq!(values(&pool), vec![1, 3]);
    }

    /// A successful step's writes are kept, including a nested step's.
    #[test]
    fn a_successful_step_is_kept_and_steps_nest() {
        let (_dir, pool) = test_pool();
        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");
        let conn = tx.conn();

        let outer = with_savepoint(conn, || -> Result<()> {
            insert(conn, 1)?;

            let inner = with_savepoint(conn, || -> Result<()> {
                insert(conn, 2)?;
                Err(anyhow!("inner fails"))
            })?;
            assert!(inner.is_err());

            insert(conn, 3)?;
            Ok(())
        })
        .expect("savepoints");
        assert!(outer.is_ok());

        tx.commit().expect("commit");

        assert_eq!(values(&pool), vec![1, 3]);
    }

    /// Outside a transaction there is nothing to step back to: the work runs
    /// as is, and no transaction is left open behind it.
    #[test]
    fn a_step_outside_a_transaction_runs_as_is() {
        let (_dir, pool) = test_pool();
        let conn = pool.write().expect("conn");

        let step = with_savepoint(&conn, || insert(&conn, 7)).expect("no savepoint needed");

        assert!(step.is_ok());
        assert!(!conn.in_transaction());
        assert_eq!(values(&pool), vec![7]);
    }

    /// Dropping an unsettled transaction rolls it back; committing keeps it.
    #[test]
    fn an_in_place_transaction_commits_or_rolls_back_on_drop() {
        let (_dir, pool) = test_pool();

        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");
        assert!(tx.conn().in_transaction());
        insert(tx.conn(), 1).expect("insert");
        tx.commit().expect("commit");

        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");
        insert(tx.conn(), 2).expect("insert");
        drop(tx);

        assert_eq!(values(&pool), vec![1]);
    }

    /// A lent connection is back in autocommit once its transaction settles.
    #[test]
    fn a_transaction_on_a_lent_connection_leaves_it_in_autocommit() {
        let (_dir, pool) = test_pool();
        let conn = pool.write().expect("conn");

        {
            let tx = InPlaceTransaction::begin_on(&conn).expect("begin");
            insert(tx.conn(), 1).expect("insert");
        }
        assert!(!conn.in_transaction(), "the drop rolled back");

        let tx = InPlaceTransaction::begin_on(&conn).expect("begin");
        insert(tx.conn(), 2).expect("insert");
        tx.commit().expect("commit");

        assert!(!conn.in_transaction());
        assert_eq!(values(&pool), vec![2]);
    }

    /// A write that runs far past its budget: interrupted, `SQLite` rolls back
    /// the whole transaction it ran in, not only the statement.
    fn interrupted_write(conn: &dyn DbConnection) -> Error {
        let _expired = StatementDeadlineScope::bound_to(Some(Instant::now()));

        conn.execute(
            "INSERT INTO t (x) SELECT x FROM (WITH RECURSIVE c(x) AS \
             (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 500000) SELECT x FROM c)",
            &[],
        )
        .expect_err("the write is interrupted")
    }

    /// Regression: an interrupted write made `SQLite` roll the whole in-place
    /// transaction back and return the connection to autocommit — a caller
    /// that caught the error and carried on (a hook's `pcall`) then
    /// committed every later statement on its own, while the transaction's
    /// earlier writes were gone. Every later statement is refused, the commit
    /// fails, and nothing of the transaction persists.
    #[test]
    fn statements_after_the_database_ended_an_in_place_transaction_are_refused() {
        let (_dir, pool) = test_pool();
        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");
        let conn = tx.conn();

        insert(conn, 1).expect("before");
        assert!(interrupted_write(conn).is::<StatementTimedOut>());

        let step = with_savepoint(conn, || insert(conn, 2));
        assert!(
            step.is_err() || step.is_ok_and(|r| r.is_err()),
            "a later step is refused"
        );
        assert!(insert(conn, 3).is_err(), "a later statement is refused");
        assert!(tx.commit().is_err(), "the commit reports the loss");

        assert_eq!(values(&pool), Vec::<i64>::new());
    }

    /// The same for a transaction opened with `transaction_immediate`.
    #[test]
    fn statements_after_the_database_ended_a_boxed_transaction_are_refused() {
        let (_dir, pool) = test_pool();
        let mut conn = pool.write().expect("conn");
        let tx = conn.transaction_immediate().expect("begin");

        insert(&tx, 1).expect("before");
        assert!(interrupted_write(&tx).is::<StatementTimedOut>());

        assert!(insert(&tx, 3).is_err(), "a later statement is refused");
        assert!(tx.commit().is_err(), "the commit reports the loss");
        drop(conn);

        assert_eq!(values(&pool), Vec::<i64>::new());
    }

    /// The in-place transaction is IMMEDIATE on `SQLite`: it holds the write
    /// lock from its `BEGIN`, so a read-then-write inside it can never fail
    /// with `SQLITE_BUSY_SNAPSHOT` — and another writer waits for it instead.
    #[test]
    fn an_in_place_transaction_takes_the_write_lock_up_front() {
        let (_dir, pool) = test_pool();
        let tx = InPlaceTransaction::begin_owned(pool.write().expect("conn")).expect("begin");

        let other = pool.get().expect("second conn");
        let Err(e) = other.begin_in_place() else {
            panic!("a second writer must not get the lock while the first holds it");
        };

        assert!(format!("{e:#}").contains("locked"), "got: {e:#}");
        drop(tx);
    }
}
