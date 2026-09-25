//! `PostgreSQL` backend — connection, transaction, and pool implementation.
//!
//! Uses `deadpool-postgres` (async pool) with `tokio::task::block_in_place`
//! to provide the sync `DbConnection` interface expected by the rest of
//! the codebase.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Error, Result, anyhow, bail};
use deadpool::{Runtime, managed::Timeouts};
use parking_lot::Mutex;
use tokio::task::block_in_place;
use tokio_postgres::{Statement, types::Type};
use tracing::info;

mod stmt_cache;
mod timeout;
mod tx;

use stmt_cache::{
    CachedClient, CachedManager, CachedObject, CachedPool, StmtCache, cached_stmt_call,
};
use timeout::bounded;
use tx::{TxState, ensure_not_aborted};

use crate::{
    config::CrapConfig,
    core::FieldType,
    db::{
        BoxedConnection, DbConnection, DbPool, DbRow, DbValue, UpsertSpec,
        connection::{ConnectionInner, TransactionInner},
        deadline::{configured_timeout, statement_budget},
        pool::PoolBackend,
    },
};

/// Widen `INTEGER` column types to `BIGINT` for Postgres DDL, but ONLY outside
/// single-quoted string literals — a blind `sql.replace(" INTEGER", " BIGINT")`
/// would also rewrite a literal such as `DEFAULT 'AN INTEGER'`, persisting a
/// corrupted default. `INTEGER` as a type token always sits outside quotes in
/// our generated DDL, so we replace only in the even (unquoted) segments when
/// splitting on `'`.
fn pg_widen_integer(sql: &str) -> String {
    sql.split('\'')
        .enumerate()
        .map(|(i, seg)| {
            if i % 2 == 0 {
                seg.replace(" INTEGER", " BIGINT")
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("'")
}

#[cfg(test)]
mod widen_tests {
    use super::pg_widen_integer;

    #[test]
    fn widens_type_token_but_not_string_literal() {
        assert_eq!(pg_widen_integer("x INTEGER NOT NULL"), "x BIGINT NOT NULL");
        assert_eq!(
            pg_widen_integer("a INTEGER, b INTEGER)"),
            "a BIGINT, b BIGINT)"
        );
        // A string literal containing " INTEGER" must survive verbatim.
        assert_eq!(
            pg_widen_integer("v TEXT DEFAULT 'AN INTEGER'"),
            "v TEXT DEFAULT 'AN INTEGER'"
        );
        assert_eq!(
            pg_widen_integer("a INTEGER, b TEXT DEFAULT 'an INTEGER value'"),
            "a BIGINT, b TEXT DEFAULT 'an INTEGER value'"
        );
    }
}

// ── Shared trait methods (non-query) ─────────────────────────────────────

/// Methods that don't depend on the client type — implemented identically
/// for both `PgConnection` and `PgTransaction`.
macro_rules! pg_shared_methods {
    () => {
        fn placeholder(&self, n: usize) -> String {
            pg_placeholder(n)
        }

        fn now_expr(&self) -> &'static str {
            pg_now_expr()
        }

        fn greatest_expr(&self, a: &str, b: &str) -> String {
            pg_greatest_expr(a, b)
        }

        fn kind(&self) -> &'static str {
            "postgres"
        }

        fn table_exists(&self, name: &str) -> Result<bool> {
            let row = self.query_one(
                "SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = 'public' AND table_name = $1",
                &[DbValue::Text(name.to_string())],
            )?;
            Ok(row.is_some())
        }

        fn get_table_columns(&self, table: &str) -> Result<HashSet<String>> {
            let rows = self.query_all(
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1",
                &[DbValue::Text(table.to_string())],
            )?;
            Ok(rows
                .iter()
                .filter_map(|r| r.get_string("column_name").ok())
                .collect())
        }

        fn get_table_column_types(&self, table: &str) -> Result<HashMap<String, String>> {
            let rows = self.query_all(
                "SELECT column_name, data_type FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1",
                &[DbValue::Text(table.to_string())],
            )?;
            let mut map = HashMap::new();
            for row in &rows {
                if let (Ok(name), Ok(dtype)) =
                    (row.get_string("column_name"), row.get_string("data_type"))
                {
                    map.insert(name, dtype);
                }
            }
            Ok(map)
        }

        fn index_names(&self, table: &str, prefix: &str) -> Result<Vec<String>> {
            let rows = self.query_all(
                "SELECT indexname FROM pg_indexes \
                 WHERE tablename = $1 AND indexname LIKE $2",
                &[
                    DbValue::Text(table.to_string()),
                    DbValue::Text(format!("{prefix}%")),
                ],
            )?;
            // `LIKE` reads a `_` in the prefix as any character: keep only
            // the names that really start with `prefix`.
            Ok(rows
                .iter()
                .filter_map(|r| r.get_string("indexname").ok())
                .filter(|name| name.starts_with(prefix))
                .collect())
        }

        fn timestamp_column_default(&self) -> &'static str {
            "TEXT DEFAULT to_char(NOW(), 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        }

        fn timestamp_column_type(&self) -> &'static str {
            "TEXT"
        }

        fn column_type_for(&self, ft: &FieldType) -> &'static str {
            pg_column_type_for(ft)
        }

        fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
            pg_date_offset_expr(seconds, param_pos)
        }

        fn json_extract_expr(&self, column: &str, field: &str) -> String {
            pg_json_extract_expr(column, field)
        }

        fn json_number_cast(&self, expr: &str) -> String {
            pg_json_number_cast(expr)
        }

        fn json_checkbox_cast(&self, expr: &str) -> String {
            pg_json_checkbox_cast(expr)
        }

        fn lock_row(&self, table: &str, id: &str) -> Result<()> {
            self.execute(
                &format!(
                    "SELECT 1 FROM \"{table}\" WHERE id = {} FOR UPDATE",
                    self.placeholder(1)
                ),
                &[DbValue::Text(id.to_string())],
            )?;
            Ok(())
        }

        fn advisory_xact_lock(&self, key: i64) -> Result<()> {
            self.execute(
                &format!("SELECT pg_advisory_xact_lock({})", self.placeholder(1)),
                &[DbValue::Integer(key)],
            )?;
            Ok(())
        }

        fn json_each_source(&self, source: &str, alias: &str) -> String {
            pg_json_each_source(source, alias)
        }

        fn text_after(&self, expr: &str, separator: &str) -> String {
            pg_text_after(expr, separator)
        }

        fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
            pg_build_insert_ignore(table, columns, values)
        }

        fn build_upsert(&self, spec: &UpsertSpec<'_>) -> String {
            pg_build_upsert(spec)
        }

        fn supports_fts(&self) -> bool {
            true
        }

        fn like_operator(&self) -> &'static str {
            "ILIKE"
        }

        fn list_user_tables(&self) -> Result<Vec<String>> {
            let rows = self.query_all(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
                &[],
            )?;
            Ok(rows
                .iter()
                .filter_map(|r| r.get_string("table_name").ok())
                .collect())
        }

        fn supports_drop_column(&self) -> bool {
            true
        }

        fn vacuum_into(&self, _dest: &std::path::Path) -> Result<()> {
            bail!(
                "VACUUM INTO is not supported for PostgreSQL. \
                 Use pg_dump for database backups."
            )
        }

        fn sidecar_extensions(&self) -> &[&str] {
            &[]
        }

        fn normalize_timestamp(&self, ts: &str) -> String {
            pg_normalize_timestamp(ts)
        }
    };
}

// ── Pure SQL builders ────────────────────────────────────────────────────
//
// Extracted from `pg_shared_methods!` so they are unit-testable without a
// live connection (the macro stamps the trait methods into every connection
// /transaction impl, where they would otherwise only run against a real PG).
// Mirrors the `sqlite_*` free-function layout in `sqlite.rs`.

fn pg_placeholder(n: usize) -> String {
    format!("${n}")
}

fn pg_now_expr() -> &'static str {
    "to_char(NOW(), 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
}

fn pg_greatest_expr(a: &str, b: &str) -> String {
    format!("GREATEST({a}, {b})")
}

fn pg_column_type_for(ft: &FieldType) -> &'static str {
    match ft {
        FieldType::Number => "DOUBLE PRECISION",
        FieldType::Checkbox => "SMALLINT",
        _ => "TEXT",
    }
}

/// Build an offset-timestamp expression `now - seconds`, matching the
/// backend-agnostic contract (positive `seconds` → a timestamp in the past,
/// negative → future). `SQLite`'s `sqlite_date_offset_expr` computes the same
/// `now - seconds`; this must stay in lockstep with it.
///
/// `make_interval(secs => $n)` takes a numeric seconds argument, which
/// `tokio-postgres` binds from a `DbValue::Real`.
fn pg_date_offset_expr(seconds: i64, param_pos: usize) -> (String, DbValue) {
    // Callers pass token/session/retention windows (hours/days as seconds,
    // far below 2^53), so the i64→f64 conversion is lossless in practice.
    #[allow(clippy::cast_precision_loss)]
    let secs_real = seconds as f64;
    let sql = format!(
        "to_char(NOW() - make_interval(secs => ${param_pos}), \
         'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
    );
    (sql, DbValue::Real(secs_real))
}

fn pg_json_extract_expr(column: &str, field: &str) -> String {
    // `field` may be a dotted path (`meta.title`) for a Group sub-field nested
    // inside a Blocks/Array value. Postgres `->>` takes a single key, so a
    // dotted field must use the `#>>'{a,b}'` path form (SQLite's `json_extract`
    // walks the dotted path natively). A single segment yields `#>>'{field}'`,
    // which is equivalent to `->>'field'`. Segments are validated identifiers
    // upstream (`is_valid_identifier`), so the path literal carries no
    // injection surface.
    let path = field.split('.').collect::<Vec<_>>().join(",");
    format!("{column}::jsonb#>>'{{{path}}}'")
}

/// Cast a JSON-extract (`#>>`/`->>` yield `text`) to a number so a `Number`
/// sub-field compares numerically instead of `text <op> float8` erroring or
/// comparing lexically. `double precision` matches the `Number` column type and
/// the operand, which binds as `f64`/`float8` (a `numeric` cast would make PG
/// infer the operand as `numeric`, which the `f64` binder can't produce).
fn pg_json_number_cast(expr: &str) -> String {
    format!("({expr})::double precision")
}

/// Map a JSON-extract of a `Checkbox` (`#>>` yields the text `'true'` /
/// `'false'`) to the integer its operand binds as — `1`/`0`, as `SQLite`'s
/// `json_extract` reads it; a stored `1`/`0` maps the same way, anything else
/// to `NULL`.
fn pg_json_checkbox_cast(expr: &str) -> String {
    format!(
        "(CASE ({expr}) WHEN 'true' THEN 1 WHEN '1' THEN 1 WHEN 'false' THEN 0 WHEN '0' THEN 0 END)"
    )
}

/// The `FROM` item that expands a JSON array into one row per element.
///
/// `source` is frequently a [`pg_json_extract_expr`] result, and `#>>` yields
/// `text`; Postgres has no implicit text→jsonb cast, so without the explicit
/// one every filter descending into an array or blocks nested inside a row
/// fails with "function `jsonb_array_elements_text(text)` does not exist". The
/// cast is a no-op on a `source` that is already `jsonb`. `SQLite`'s
/// `json_each(json_extract(…))` composes without it, which is why this only
/// ever showed on Postgres.
fn pg_json_each_source(source: &str, alias: &str) -> String {
    format!("jsonb_array_elements_text(({source})::jsonb) AS {alias}")
}

/// The text of `expr` after the first `separator`: `strpos` is 0 when there is
/// none, so `substr` then starts at the first character.
fn pg_text_after(expr: &str, separator: &str) -> String {
    let separator = separator.replace('\'', "''");

    format!("substr({expr}, strpos({expr}, '{separator}') + 1)")
}

fn pg_build_insert_ignore(table: &str, columns: &str, values: &str) -> String {
    format!("INSERT INTO \"{table}\" ({columns}) VALUES ({values}) ON CONFLICT DO NOTHING")
}

fn pg_build_upsert(spec: &UpsertSpec<'_>) -> String {
    let UpsertSpec {
        table,
        columns,
        values,
        key_col,
        guard,
    } = *spec;

    let cols = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let updates = columns
        .iter()
        .filter(|c| **c != key_col)
        .map(|c| format!("\"{c}\" = EXCLUDED.\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let guard = guard.map_or_else(String::new, |g| format!(" WHERE {g}"));

    format!(
        "INSERT INTO \"{table}\" ({cols}) VALUES ({values}) \
         ON CONFLICT (\"{key_col}\") DO UPDATE SET {updates}{guard}"
    )
}

fn pg_normalize_timestamp(ts: &str) -> String {
    ts.to_string()
}

// ── Pool ─────────────────────────────────────────────────────────────────

/// Create a `PostgreSQL` connection pool from config.
///
/// # Errors
///
/// Returns an error when `database.url` is missing from the config,
/// when the URL string fails to parse as a `tokio_postgres::Config`,
/// or when the bb8 pool builder rejects the configured pool size.
pub fn create_pool(config: &CrapConfig) -> Result<DbPool> {
    let url = config
        .database
        .url
        .as_ref()
        .map(crate::config::DbUrl::as_str)
        .ok_or_else(|| anyhow!("database.url is required for postgres backend"))?;

    let pg_config: tokio_postgres::Config = url.parse().context("Invalid postgres URL")?;
    let mgr = CachedManager { config: pg_config };

    // Every wait is bounded. `PgPoolBackend::get` blocks a Tokio worker thread
    // on this future, so an unbounded wait parks that worker for as long as the
    // pool stays exhausted or the server stays unreachable — a saturated pool
    // would take the runtime down with it instead of failing requests. The
    // `SQLite` pool bounds the same wait with r2d2's `connection_timeout`; both
    // read the one configured value. A `Runtime` is required for the timeouts
    // to be applied at all (without it deadpool answers `NoRuntimeSpecified`).
    let timeout = Duration::from_secs(config.database.connection_timeout);

    let pool = CachedPool::builder(mgr)
        .max_size(config.database.pool_max_size as usize)
        .runtime(Runtime::Tokio1)
        .timeouts(Timeouts {
            wait: Some(timeout),
            create: Some(timeout),
            recycle: Some(timeout),
        })
        .build()
        .context("Failed to create Postgres connection pool")?;

    info!(
        "Postgres pool created (max_size={}, timeout={}s, statement cache enabled)",
        config.database.pool_max_size, config.database.connection_timeout
    );

    Ok(DbPool::from_backend(Arc::new(PgPoolBackend {
        pool,
        statement_timeout: configured_timeout(config.database.statement_timeout),
    })))
}

struct PgPoolBackend {
    pool: CachedPool,
    /// `[database] statement_timeout` (`None`: off).
    statement_timeout: Option<Duration>,
}

impl PoolBackend for PgPoolBackend {
    fn get(&self) -> Result<BoxedConnection> {
        let obj = block_in_place(|| tokio::runtime::Handle::current().block_on(self.pool.get()))
            // The typed pool error stays the source of the chain: formatting it
            // into a message would flatten it to one line and drop the cause
            // underneath (the driver's "connection refused", say), which both
            // the log and the transient/internal classification read.
            .map_err(|e| Error::new(e).context("Failed to get Postgres connection"))?;

        Ok(BoxedConnection::new(Box::new(PgConnection {
            inner: obj,
            statement_timeout: self.statement_timeout,
        })))
    }

    fn kind(&self) -> &'static str {
        "postgres"
    }
}

// ── Connection ───────────────────────────────────────────────────────────

pub struct PgConnection {
    inner: CachedObject,
    statement_timeout: Option<Duration>,
}

impl ConnectionInner for PgConnection {
    fn transaction_boxed(&mut self) -> Result<Box<dyn TransactionInner + '_>> {
        // Splitting borrow on CachedClient: tx needs &mut client, cache stays
        // shared. The Transaction itself implements GenericClient and has
        // its own prepare() — no need to also hold a &Client.
        let statement_timeout = self.statement_timeout;
        let cached: &mut CachedClient = &mut self.inner;
        let cache = &cached.cache;
        let state = &cached.tx;

        if state.in_place() {
            bail!("a transaction is already open on this connection");
        }

        let tx = block_in_place(|| {
            tokio::runtime::Handle::current().block_on(cached.client.transaction())
        })
        .context("Failed to begin transaction")?;

        state.began(false);

        Ok(Box::new(PgTransaction {
            inner: tx,
            cache,
            state,
            statement_timeout,
        }))
    }

    fn transaction_immediate_boxed(&mut self) -> Result<Box<dyn TransactionInner + '_>> {
        // Postgres uses MVCC — no need for IMMEDIATE mode.
        self.transaction_boxed()
    }
}

/// Generate the query methods of `DbConnection` that route through
/// `cached_prepare` so we benefit from the per-connection statement cache
/// on every call. Both `PgConnection` and `PgTransaction` use this.
///
/// Inputs:
/// - `$exec_expr`: `self -> &impl GenericClient` accessor — used for the
///   actual execute/query AND for `prepare()`. Both Client and Transaction
///   have `prepare()`; a Statement is connection-bound and survives its
///   transaction's commit and its rollback alike (only portals are
///   transaction-scoped), so the cache lives at the connection level.
/// - `$cache_expr`: `self -> StmtCache<'_>`.
macro_rules! pg_query_methods {
    (
        |$s:ident| exec = $exec_expr:expr,
        cache = $cache_expr:expr,
        state = $state_expr:expr,
        timeout = $timeout_expr:expr
    ) => {
        fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
            let pg_params = to_pg_params(params);
            let owned_refs = pg_param_refs(&pg_params);
            // A shared reference, so the `run` closure stays callable twice
            // (the retry) instead of moving the vector into its first future.
            let refs = &owned_refs;
            let $s = self;
            let exec = $exec_expr;
            let cache = $cache_expr;
            let budget = statement_budget($timeout_expr);
            let cancel = exec.cancel_token();
            let count = block_in_place(|| {
                tokio::runtime::Handle::current().block_on(bounded(
                    budget,
                    cancel,
                    cached_stmt_call(
                        &cache,
                        sql,
                        || exec.prepare(sql),
                        |stmt| async move { exec.execute(&stmt, refs).await },
                    ),
                ))
            })
            .with_context(|| format!("execute failed: {sql}"));
            $state_expr.record(&count);
            let count = count?;
            // tokio-postgres returns the row count as u64; we report it as
            // usize. On 32-bit targets a single UPDATE / DELETE returning
            // more than 4 billion rows is implausible, but saturate
            // explicitly rather than silently truncate.
            Ok(usize::try_from(count).unwrap_or(usize::MAX))
        }

        fn execute_batch(&self, sql: &str) -> Result<()> {
            // batch_execute uses simple-query protocol (multi-statement,
            // no params, no caching). Used for setup/migration SQL where
            // the savings of caching wouldn't apply.
            let $s = self;
            let exec = $exec_expr;
            let budget = statement_budget($timeout_expr);
            let result = block_in_place(|| {
                tokio::runtime::Handle::current().block_on(bounded(
                    budget,
                    exec.cancel_token(),
                    exec.batch_execute(sql),
                ))
            })
            .with_context(|| format!("execute_batch failed: {sql}"));
            $state_expr.record(&result);
            result
        }

        fn execute_ddl(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
            self.execute(&pg_widen_integer(sql), params)
        }

        fn execute_batch_ddl(&self, sql: &str) -> Result<()> {
            self.execute_batch(&pg_widen_integer(sql))
        }

        fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
            let pg_params = to_pg_params(params);
            let owned_refs = pg_param_refs(&pg_params);
            let refs = &owned_refs;
            let $s = self;
            let exec = $exec_expr;
            let cache = $cache_expr;
            let budget = statement_budget($timeout_expr);
            let cancel = exec.cancel_token();
            let rows = block_in_place(|| {
                tokio::runtime::Handle::current().block_on(bounded(
                    budget,
                    cancel,
                    cached_stmt_call(
                        &cache,
                        sql,
                        || exec.prepare(sql),
                        |stmt| async move { exec.query(&stmt, refs).await },
                    ),
                ))
            })
            .with_context(|| format!("query failed: {sql}"));
            $state_expr.record(&rows);
            let rows = rows?;
            Ok(rows.iter().map(pg_row_to_dbrow).collect())
        }

        fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
            let pg_params = to_pg_params(params);
            let owned_refs = pg_param_refs(&pg_params);
            let refs = &owned_refs;
            let $s = self;
            let exec = $exec_expr;
            let cache = $cache_expr;
            let budget = statement_budget($timeout_expr);
            let cancel = exec.cancel_token();
            let row = block_in_place(|| {
                tokio::runtime::Handle::current().block_on(bounded(
                    budget,
                    cancel,
                    cached_stmt_call(
                        &cache,
                        sql,
                        || exec.prepare(sql),
                        |stmt| async move { exec.query_opt(&stmt, refs).await },
                    ),
                ))
            })
            .with_context(|| format!("query_one failed: {sql}"));
            $state_expr.record(&row);
            let row = row?;
            Ok(row.as_ref().map(pg_row_to_dbrow))
        }
    };
}

impl DbConnection for PgConnection {
    pg_query_methods!(
        |this| exec = &this.inner.client,
        cache = this.stmt_cache(),
        state = this.inner.tx,
        timeout = this.statement_timeout
    );
    pg_shared_methods!();

    fn in_transaction(&self) -> bool {
        self.inner.tx.in_place()
    }

    fn begin_in_place(&self) -> Result<()> {
        if self.inner.tx.in_place() {
            bail!("a transaction is already open on this connection");
        }

        self.batch("BEGIN").context("Failed to begin transaction")?;
        self.inner.tx.began(true);

        Ok(())
    }

    fn commit_in_place(&self) -> Result<()> {
        if !self.inner.tx.in_place() {
            bail!("no transaction is open on this connection");
        }

        let checked = block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(ensure_not_aborted(&self.inner.client, &self.inner.tx))
        });

        if let Err(e) = checked {
            self.rollback_in_place()?;

            return Err(e);
        }

        self.batch("COMMIT")
            .context("Failed to commit transaction")?;
        self.inner.tx.settled();

        Ok(())
    }

    fn rollback_in_place(&self) -> Result<()> {
        self.batch("ROLLBACK")
            .context("Failed to roll back transaction")?;
        self.inner.tx.settled();

        Ok(())
    }
}

impl PgConnection {
    /// The statement cache as this connection sees it right now: inside an
    /// in-place transaction a stale statement must not be retried (see
    /// [`StmtCache`]).
    fn stmt_cache(&self) -> StmtCache<'_> {
        if self.inner.tx.in_place() {
            return StmtCache::in_transaction(&self.inner.cache);
        }

        StmtCache::connection(&self.inner.cache)
    }

    /// Run one transaction-control statement over the simple-query protocol.
    fn batch(&self, sql: &str) -> Result<()> {
        block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.inner.client.batch_execute(sql))
        })
        .map_err(Error::new)
    }
}

// ── Transaction ──────────────────────────────────────────────────────────

pub struct PgTransaction<'conn> {
    inner: tokio_postgres::Transaction<'conn>,
    cache: &'conn Mutex<HashMap<String, Statement>>,
    state: &'conn TxState,
    statement_timeout: Option<Duration>,
}

impl TransactionInner for PgTransaction<'_> {
    fn commit_inner(self: Box<Self>) -> Result<()> {
        let Self { inner, state, .. } = *self;

        // An aborted transaction is refused before `COMMIT`: dropping `inner`
        // then rolls it back, instead of the server answering `COMMIT` with a
        // `ROLLBACK` the driver reports as success.
        block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                ensure_not_aborted(&inner, state).await?;

                inner.commit().await.context("Failed to commit transaction")
            })
        })
    }
}

impl DbConnection for PgTransaction<'_> {
    pg_query_methods!(
        |this| exec = &this.inner,
        cache = StmtCache::in_transaction(this.cache),
        state = this.state,
        timeout = this.statement_timeout
    );
    pg_shared_methods!();

    fn in_transaction(&self) -> bool {
        true
    }

    fn begin_in_place(&self) -> Result<()> {
        bail!("a transaction is already open on this connection")
    }

    fn commit_in_place(&self) -> Result<()> {
        bail!("this transaction is settled by its owner, not in place")
    }

    fn rollback_in_place(&self) -> Result<()> {
        bail!("this transaction is settled by its owner, not in place")
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// An integer parameter that adapts to the statement's inferred Postgres
/// type. `DbValue::Integer` is an `i64`, but tokio-postgres type-checks
/// params strictly — a plain `i64` binding is rejected for an INT2/INT4
/// target (e.g. the SMALLINT checkbox columns). Serializes per the expected
/// type with range checks.
#[derive(Debug)]
struct AdaptiveInt(i64);

impl tokio_postgres::types::ToSql for AdaptiveInt {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut bytes::BytesMut,
    ) -> std::result::Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>>
    {
        match *ty {
            Type::INT2 => i16::try_from(self.0)?.to_sql(ty, out),
            Type::INT4 => i32::try_from(self.0)?.to_sql(ty, out),
            // A Number field is DOUBLE PRECISION; a whole value read from it
            // normalizes to a JSON integer (`real_to_json_number`), so a keyset
            // cursor comparand for a numeric sort column arrives here as an i64
            // that must bind against a FLOAT8 target. The value originated from
            // an f64 column, so it is exactly representable.
            Type::FLOAT8 => {
                #[allow(clippy::cast_precision_loss)]
                let as_float = self.0 as f64;
                as_float.to_sql(ty, out)
            }
            _ => self.0.to_sql(ty, out),
        }
    }

    fn accepts(ty: &Type) -> bool {
        matches!(*ty, Type::INT2 | Type::INT4 | Type::INT8 | Type::FLOAT8)
    }

    tokio_postgres::types::to_sql_checked!();
}

/// Type-agnostic SQL NULL. Binding `None::<String>` (the old approach)
/// declares the parameter as TEXT, which tokio-postgres rejects against any
/// non-text column — e.g. a NULL checkbox sub-field in an array row hits an
/// INT2 column and fails with "cannot convert … Option<String> and the
/// Postgres type int2". SQL NULL carries no type, so accept every column
/// type and always serialize as NULL.
#[derive(Debug)]
struct AdaptiveNull;

impl tokio_postgres::types::ToSql for AdaptiveNull {
    fn to_sql(
        &self,
        _ty: &Type,
        _out: &mut bytes::BytesMut,
    ) -> std::result::Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>>
    {
        Ok(tokio_postgres::types::IsNull::Yes)
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }

    tokio_postgres::types::to_sql_checked!();
}

/// Convert `DbValue` slice to tokio-postgres parameter boxes.
fn to_pg_params(params: &[DbValue]) -> Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> {
    params
        .iter()
        .map(|v| -> Box<dyn tokio_postgres::types::ToSql + Sync + Send> {
            match v {
                DbValue::Null => Box::new(AdaptiveNull),
                DbValue::Integer(i) => Box::new(AdaptiveInt(*i)),
                DbValue::Real(f) => Box::new(*f),
                DbValue::Text(s) => Box::new(s.clone()),
                DbValue::Blob(b) => Box::new(b.clone()),
            }
        })
        .collect()
}

/// Build parameter reference slice from boxed params.
fn pg_param_refs(
    params: &[Box<dyn tokio_postgres::types::ToSql + Sync + Send>],
) -> Vec<&(dyn tokio_postgres::types::ToSql + Sync)> {
    params
        .iter()
        .map(|b| &**b as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect()
}

/// Convert a tokio-postgres row to a `DbRow`.
fn pg_row_to_dbrow(row: &tokio_postgres::Row) -> DbRow {
    let mut columns = Vec::with_capacity(row.columns().len());
    let mut values = Vec::with_capacity(row.columns().len());

    for (i, col) in row.columns().iter().enumerate() {
        columns.push(col.name().to_string());
        values.push(pg_column_to_dbvalue(row, i, col.type_()));
    }

    DbRow::new(columns, values)
}

/// Extract a single column value, dispatching on Postgres type.
fn pg_column_to_dbvalue(row: &tokio_postgres::Row, idx: usize, ty: &Type) -> DbValue {
    match *ty {
        Type::BOOL => match row.try_get::<_, Option<bool>>(idx) {
            Ok(Some(b)) => DbValue::Integer(i64::from(b)),
            _ => DbValue::Null,
        },
        Type::INT2 => match row.try_get::<_, Option<i16>>(idx) {
            Ok(Some(v)) => DbValue::Integer(i64::from(v)),
            _ => DbValue::Null,
        },
        Type::INT4 => match row.try_get::<_, Option<i32>>(idx) {
            Ok(Some(v)) => DbValue::Integer(i64::from(v)),
            _ => DbValue::Null,
        },
        Type::INT8 => match row.try_get::<_, Option<i64>>(idx) {
            Ok(Some(v)) => DbValue::Integer(v),
            _ => DbValue::Null,
        },
        Type::FLOAT4 => match row.try_get::<_, Option<f32>>(idx) {
            Ok(Some(v)) => DbValue::Real(f64::from(v)),
            _ => DbValue::Null,
        },
        Type::FLOAT8 => match row.try_get::<_, Option<f64>>(idx) {
            Ok(Some(v)) => DbValue::Real(v),
            _ => DbValue::Null,
        },
        Type::BYTEA => match row.try_get::<_, Option<Vec<u8>>>(idx) {
            Ok(Some(v)) => DbValue::Blob(v),
            _ => DbValue::Null,
        },
        Type::JSON | Type::JSONB => match row.try_get::<_, Option<serde_json::Value>>(idx) {
            Ok(Some(v)) => DbValue::Text(v.to_string()),
            _ => DbValue::Null,
        },
        // Everything else (TEXT, VARCHAR, etc.) → Text
        _ => match row.try_get::<_, Option<String>>(idx) {
            Ok(Some(v)) => DbValue::Text(v),
            _ => DbValue::Null,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_uses_dollar_n() {
        assert_eq!(pg_placeholder(1), "$1");
        assert_eq!(pg_placeholder(42), "$42");
    }

    #[test]
    fn now_expr_formats_iso_utc() {
        // Mirrors sqlite's now_expr test; pins the ISO-8601 `…Z` shape so the
        // two backends stay format-compatible.
        assert_eq!(
            pg_now_expr(),
            "to_char(NOW(), 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        );
    }

    /// Regression: a checkbox inside a row's JSON compared its `#>>` text
    /// with the integer operand — a parameter error. The text maps to the
    /// integer the operand binds as.
    #[test]
    fn json_checkbox_cast_maps_the_stored_text_to_one_and_zero() {
        assert_eq!(
            pg_json_checkbox_cast("t.data::jsonb#>>'{done}'"),
            "(CASE (t.data::jsonb#>>'{done}') WHEN 'true' THEN 1 WHEN '1' THEN 1 \
             WHEN 'false' THEN 0 WHEN '0' THEN 0 END)"
        );
    }

    #[test]
    fn greatest_expr_wraps_in_greatest() {
        assert_eq!(pg_greatest_expr("a", "b"), "GREATEST(a, b)");
    }

    #[test]
    fn column_type_maps_number_checkbox_else_text() {
        assert_eq!(pg_column_type_for(&FieldType::Number), "DOUBLE PRECISION");
        assert_eq!(pg_column_type_for(&FieldType::Checkbox), "SMALLINT");
        assert_eq!(pg_column_type_for(&FieldType::Text), "TEXT");
    }

    /// Regression: the offset must SUBTRACT (`now - seconds`), matching
    /// `SQLite` and every caller (retention/purge/since use positive = past). A `+`
    /// here silently made "older than" queries match future rows → mass
    /// deletion, and inverted retry backoff.
    #[test]
    fn date_offset_expr_subtracts_the_interval() {
        let (sql, param) = pg_date_offset_expr(30, 1);
        assert!(
            sql.contains("NOW() - make_interval(secs => $1)"),
            "offset must be now - seconds, got: {sql}"
        );
        assert!(
            !sql.contains('+'),
            "offset must not add the interval: {sql}"
        );
        assert_eq!(param, DbValue::Real(30.0));
    }

    #[test]
    fn date_offset_expr_negative_input_is_future_via_same_subtraction() {
        // now - (-delay) = now + delay (future). Same SQL, negative param.
        let (sql, param) = pg_date_offset_expr(-30, 9);
        assert!(sql.contains("NOW() - make_interval(secs => $9)"));
        assert_eq!(param, DbValue::Real(-30.0));
    }

    #[test]
    fn json_extract_and_each_use_jsonb() {
        assert_eq!(
            pg_json_extract_expr("data", "title"),
            "data::jsonb#>>'{title}'"
        );
        assert_eq!(
            pg_json_extract_expr("data", "meta.title"),
            "data::jsonb#>>'{meta,title}'"
        );
        assert_eq!(
            pg_json_each_source("col", "x"),
            "jsonb_array_elements_text((col)::jsonb) AS x"
        );
    }

    /// The two JSON expressions must compose: a filter descending into an
    /// array/blocks nested inside a row builds the `json_each` source *from* a
    /// `json_extract_expr`, and `#>>` yields `text`. Without the cast Postgres
    /// has no `jsonb_array_elements_text(text)` and every such filter is a 500.
    #[test]
    fn json_each_source_accepts_a_json_extract_expr_as_its_source() {
        let source = pg_json_extract_expr("posts_content.data", "items");
        let each = pg_json_each_source(&source, "e0");

        assert_eq!(
            each,
            "jsonb_array_elements_text((posts_content.data::jsonb#>>'{items}')::jsonb) AS e0"
        );
        assert!(
            each.ends_with(")::jsonb) AS e0"),
            "the text-yielding source must be cast back to jsonb: {each}"
        );
    }

    /// The text after a separator uses functions every supported Postgres
    /// has (`strpos`, `substr`), and a quote in the separator can't end the
    /// literal.
    #[test]
    fn text_after_splits_at_the_first_separator() {
        assert_eq!(
            pg_text_after("crap_el.value", "/"),
            "substr(crap_el.value, strpos(crap_el.value, '/') + 1)"
        );
        assert_eq!(pg_text_after("x", "'"), "substr(x, strpos(x, '''') + 1)");
    }

    #[test]
    fn insert_ignore_uses_on_conflict_do_nothing() {
        assert_eq!(
            pg_build_insert_ignore("t", "a, b", "$1, $2"),
            "INSERT INTO \"t\" (a, b) VALUES ($1, $2) ON CONFLICT DO NOTHING"
        );
    }

    #[test]
    fn upsert_excludes_key_column_from_update_set() {
        // `id` is the conflict key and must not appear in the DO UPDATE SET.
        let spec = UpsertSpec::builder("t", "id")
            .columns(&["id", "name"], "$1, $2")
            .build();

        assert_eq!(
            pg_build_upsert(&spec),
            "INSERT INTO \"t\" (\"id\", \"name\") VALUES ($1, $2) \
             ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
        );
    }

    /// A guard turns the upsert into a claim: the row is written when absent,
    /// and overwritten only where the stored row still satisfies the predicate.
    /// The predicate names the table so it reads the row already stored, not
    /// the one being proposed.
    #[test]
    fn a_guarded_upsert_conditions_the_update_half() {
        let spec = UpsertSpec::builder("t", "slug")
            .columns(&["slug", "fired_at"], "$1, $2")
            .guard("t.fired_at <= $3")
            .build();

        assert_eq!(
            pg_build_upsert(&spec),
            "INSERT INTO \"t\" (\"slug\", \"fired_at\") VALUES ($1, $2) \
             ON CONFLICT (\"slug\") DO UPDATE SET \"fired_at\" = EXCLUDED.\"fired_at\" \
             WHERE t.fired_at <= $3"
        );
    }

    #[test]
    fn normalize_timestamp_is_passthrough() {
        assert_eq!(
            pg_normalize_timestamp("2026-01-01T00:00:00.000Z"),
            "2026-01-01T00:00:00.000Z"
        );
    }

    /// `AdaptiveInt` must serialize per the statement's expected type —
    /// a plain i64 binding is rejected by tokio-postgres for INT2/INT4
    /// targets (SMALLINT checkbox columns).
    #[test]
    fn adaptive_int_serializes_per_expected_type() {
        use tokio_postgres::types::ToSql;

        // accepts all three integer widths
        assert!(<AdaptiveInt as ToSql>::accepts(&Type::INT2));
        assert!(<AdaptiveInt as ToSql>::accepts(&Type::INT4));
        assert!(<AdaptiveInt as ToSql>::accepts(&Type::INT8));
        assert!(!<AdaptiveInt as ToSql>::accepts(&Type::TEXT));

        // Regression: a keyset cursor comparand for a Number (DOUBLE PRECISION)
        // sort column arrives as an i64 and must bind against FLOAT8 — without
        // this, numeric-sort pagination errored on Postgres at whole-number
        // boundaries (`cannot convert … int … float8`).
        assert!(<AdaptiveInt as ToSql>::accepts(&Type::FLOAT8));

        // FLOAT8 encoding matches a native f64 (8 bytes)
        let mut ours = bytes::BytesMut::new();
        AdaptiveInt(42).to_sql(&Type::FLOAT8, &mut ours).unwrap();
        let mut native = bytes::BytesMut::new();
        42f64.to_sql(&Type::FLOAT8, &mut native).unwrap();
        assert_eq!(ours, native);

        // INT2 encoding matches a native i16 (2 bytes)
        let mut ours = bytes::BytesMut::new();
        AdaptiveInt(1).to_sql(&Type::INT2, &mut ours).unwrap();
        let mut native = bytes::BytesMut::new();
        1i16.to_sql(&Type::INT2, &mut native).unwrap();
        assert_eq!(ours, native);

        // INT8 encoding matches a native i64 (8 bytes)
        let mut ours = bytes::BytesMut::new();
        AdaptiveInt(1).to_sql(&Type::INT8, &mut ours).unwrap();
        let mut native = bytes::BytesMut::new();
        1i64.to_sql(&Type::INT8, &mut native).unwrap();
        assert_eq!(ours, native);

        // out-of-range for the narrow target is an error, not truncation
        let mut out = bytes::BytesMut::new();
        assert!(
            AdaptiveInt(i64::from(i16::MAX) + 1)
                .to_sql(&Type::INT2, &mut out)
                .is_err()
        );
    }

    /// Regression: a `DbValue::Null` parameter must bind against ANY column
    /// type. The old `None::<String>` binding declared TEXT and made
    /// tokio-postgres reject NULLs for non-text columns — first hit by a
    /// NULL checkbox sub-field (INT2) in an array-row INSERT, which broke
    /// the example seed migration on Postgres.
    #[test]
    fn adaptive_null_accepts_every_column_type() {
        use tokio_postgres::types::{IsNull, ToSql};

        for ty in [
            Type::INT2,
            Type::INT4,
            Type::INT8,
            Type::FLOAT8,
            Type::TEXT,
            Type::BOOL,
            Type::TIMESTAMPTZ,
        ] {
            assert!(
                <AdaptiveNull as ToSql>::accepts(&ty),
                "NULL must be accepted for {ty}"
            );
            let mut out = bytes::BytesMut::new();
            let is_null = AdaptiveNull.to_sql(&ty, &mut out).unwrap();
            assert!(
                matches!(is_null, IsNull::Yes),
                "must serialize as NULL for {ty}"
            );
            assert!(out.is_empty(), "NULL writes no payload bytes");
        }
    }
}
