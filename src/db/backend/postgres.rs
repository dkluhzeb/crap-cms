//! `PostgreSQL` backend — connection, transaction, and pool implementation.
//!
//! Uses `deadpool-postgres` (async pool) with `tokio::task::block_in_place`
//! to provide the sync `DbConnection` interface expected by the rest of
//! the codebase.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{Context as _, Result, anyhow, bail};
use deadpool::managed::{self, Metrics, RecycleResult};
use parking_lot::Mutex;
use tokio::task::block_in_place;
use tokio_postgres::{Client, NoTls, Statement, types::Type};
use tracing::{error, info};

use crate::{
    config::CrapConfig,
    core::FieldType,
    db::{
        BoxedConnection, DbConnection, DbPool, DbRow, DbValue,
        connection::{ConnectionInner, TransactionInner},
        pool::PoolBackend,
    },
};

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
            Ok(rows
                .iter()
                .filter_map(|r| r.get_string("indexname").ok())
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

        fn json_each_source(&self, source: &str, alias: &str) -> String {
            pg_json_each_source(source, alias)
        }

        fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
            pg_build_insert_ignore(table, columns, values)
        }

        fn build_upsert(
            &self,
            table: &str,
            columns: &[&str],
            values: &str,
            key_col: &str,
        ) -> String {
            pg_build_upsert(table, columns, values, key_col)
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

fn pg_json_each_source(source: &str, alias: &str) -> String {
    format!("jsonb_array_elements_text({source}) AS {alias}")
}

fn pg_build_insert_ignore(table: &str, columns: &str, values: &str) -> String {
    format!("INSERT INTO \"{table}\" ({columns}) VALUES ({values}) ON CONFLICT DO NOTHING")
}

fn pg_build_upsert(table: &str, columns: &[&str], values: &str, key_col: &str) -> String {
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
    format!(
        "INSERT INTO \"{table}\" ({cols}) VALUES ({values}) \
         ON CONFLICT (\"{key_col}\") DO UPDATE SET {updates}"
    )
}

fn pg_normalize_timestamp(ts: &str) -> String {
    ts.to_string()
}

// ── Statement-cached pool ────────────────────────────────────────────────

/// A pooled `tokio_postgres` `Client` plus a per-connection prepared-statement
/// cache. Statements are connection-bound, so the cache must live with the
/// client across pool checkouts — we achieve that by making `CachedClient`
/// the deadpool Manager's pooled `Type`.
///
/// rusqlite has the equivalent built in (`prepare_cached`); without this
/// wrapper, every postgres call re-parses the SQL on the postgres side and
/// the read-path latency is structurally higher than sqlite's even for
/// trivial queries. Caching brings postgres to feature parity.
pub struct CachedClient {
    client: Client,
    cache: Mutex<HashMap<String, Statement>>,
}

/// Custom deadpool Manager that produces `CachedClient` instances. We can't
/// use `deadpool_postgres::Manager` because its pooled `Type` is the bare
/// `tokio_postgres::Client` — there's no place to attach the cache.
pub struct CachedManager {
    config: tokio_postgres::Config,
}

impl managed::Manager for CachedManager {
    type Type = CachedClient;
    type Error = tokio_postgres::Error;

    async fn create(&self) -> std::result::Result<CachedClient, tokio_postgres::Error> {
        let (client, conn) = self.config.connect(NoTls).await?;
        // Spawn the connection driver. tokio_postgres requires this — the
        // Client is just a handle; the driver future does the actual I/O.
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("postgres connection task error: {e}");
            }
        });
        // Set the timezone once at connection creation, so all timestamp
        // expressions return UTC regardless of server config. Done here
        // (not on every checkout) so it's a one-time cost.
        client.batch_execute("SET timezone = 'UTC'").await?;
        Ok(CachedClient {
            client,
            cache: Mutex::new(HashMap::new()),
        })
    }

    async fn recycle(
        &self,
        _: &mut CachedClient,
        _: &Metrics,
    ) -> RecycleResult<tokio_postgres::Error> {
        // Fast no-op recycle. Cache + connection state preserved across
        // checkouts. We don't run `DISCARD ALL` because we don't use
        // session-local state we'd want to clear (no temp tables, no
        // advisory locks, no SET/RESET runtime params). Discarding would
        // also throw away the prepared statements — defeating the point.
        Ok(())
    }
}

type CachedPool = managed::Pool<CachedManager>;
type CachedObject = managed::Object<CachedManager>;

/// Prepare a statement (cache lookup first), then return the cached
/// `Statement` ready for `client.execute(&stmt, &params)`. The `prepare`
/// callable is supplied by the caller so this works against either a
/// `Client` or a `Transaction` (both expose `prepare(&str)`); the caller
/// closes over `sql` so the borrow stays valid for the future's lifetime.
async fn cached_prepare<F, Fut>(
    cache: &Mutex<HashMap<String, Statement>>,
    sql: &str,
    prepare: F,
) -> std::result::Result<Statement, tokio_postgres::Error>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<Statement, tokio_postgres::Error>>,
{
    if let Some(stmt) = cache.lock().get(sql).cloned() {
        return Ok(stmt);
    }
    let stmt = prepare().await?;
    cache.lock().insert(sql.to_string(), stmt.clone());
    Ok(stmt)
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

    let pool = CachedPool::builder(mgr)
        .max_size(config.database.pool_max_size as usize)
        .build()
        .context("Failed to create Postgres connection pool")?;

    info!(
        "Postgres pool created (max_size={}, statement cache enabled)",
        config.database.pool_max_size
    );

    Ok(DbPool::from_backend(Arc::new(PgPoolBackend { pool })))
}

struct PgPoolBackend {
    pool: CachedPool,
}

impl PoolBackend for PgPoolBackend {
    fn get(&self) -> Result<BoxedConnection> {
        let obj = block_in_place(|| tokio::runtime::Handle::current().block_on(self.pool.get()))
            .map_err(|e| anyhow!("Failed to get Postgres connection: {e}"))?;

        Ok(BoxedConnection::new(Box::new(PgConnection { inner: obj })))
    }

    fn kind(&self) -> &'static str {
        "postgres"
    }
}

// ── Connection ───────────────────────────────────────────────────────────

pub struct PgConnection {
    inner: CachedObject,
}

impl ConnectionInner for PgConnection {
    fn transaction_boxed(&mut self) -> Result<Box<dyn TransactionInner + '_>> {
        // Splitting borrow on CachedClient: tx needs &mut client, cache stays
        // shared. The Transaction itself implements GenericClient and has
        // its own prepare() — no need to also hold a &Client.
        let cached: &mut CachedClient = &mut self.inner;
        let cache = &cached.cache;
        let tx = block_in_place(|| {
            tokio::runtime::Handle::current().block_on(cached.client.transaction())
        })
        .context("Failed to begin transaction")?;

        Ok(Box::new(PgTransaction { inner: tx, cache }))
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
///   have `prepare()`; Statements are connection-bound and survive the
///   surrounding transaction's commit/rollback so they're safe to cache
///   at the connection level.
/// - `$cache_expr`: `self -> &Mutex<HashMap<String, Statement>>`.
macro_rules! pg_query_methods {
    (|$s:ident| exec = $exec_expr:expr, cache = $cache_expr:expr) => {
        fn execute(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
            let pg_params = to_pg_params(params);
            let refs = pg_param_refs(&pg_params);
            let $s = self;
            let exec = $exec_expr;
            let cache = $cache_expr;
            let count = block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let stmt = cached_prepare(cache, sql, || exec.prepare(sql)).await?;
                    exec.execute(&stmt, &refs).await
                })
            })
            .with_context(|| format!("execute failed: {sql}"))?;
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
            block_in_place(|| {
                tokio::runtime::Handle::current().block_on($exec_expr.batch_execute(sql))
            })
            .with_context(|| format!("execute_batch failed: {sql}"))?;
            Ok(())
        }

        fn execute_ddl(&self, sql: &str, params: &[DbValue]) -> Result<usize> {
            let adjusted = sql.replace(" INTEGER", " BIGINT");
            self.execute(&adjusted, params)
        }

        fn execute_batch_ddl(&self, sql: &str) -> Result<()> {
            let adjusted = sql.replace(" INTEGER", " BIGINT");
            self.execute_batch(&adjusted)
        }

        fn query_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<DbRow>> {
            let pg_params = to_pg_params(params);
            let refs = pg_param_refs(&pg_params);
            let $s = self;
            let exec = $exec_expr;
            let cache = $cache_expr;
            let rows = block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let stmt = cached_prepare(cache, sql, || exec.prepare(sql)).await?;
                    exec.query(&stmt, &refs).await
                })
            })
            .with_context(|| format!("query failed: {sql}"))?;
            Ok(rows.iter().map(pg_row_to_dbrow).collect())
        }

        fn query_one(&self, sql: &str, params: &[DbValue]) -> Result<Option<DbRow>> {
            let pg_params = to_pg_params(params);
            let refs = pg_param_refs(&pg_params);
            let $s = self;
            let exec = $exec_expr;
            let cache = $cache_expr;
            let row = block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    let stmt = cached_prepare(cache, sql, || exec.prepare(sql)).await?;
                    exec.query_opt(&stmt, &refs).await
                })
            })
            .with_context(|| format!("query_one failed: {sql}"))?;
            Ok(row.as_ref().map(pg_row_to_dbrow))
        }
    };
}

impl DbConnection for PgConnection {
    pg_query_methods!(|this| exec = &this.inner.client, cache = &this.inner.cache);
    pg_shared_methods!();
}

// ── Transaction ──────────────────────────────────────────────────────────

pub struct PgTransaction<'conn> {
    inner: tokio_postgres::Transaction<'conn>,
    cache: &'conn Mutex<HashMap<String, Statement>>,
}

impl TransactionInner for PgTransaction<'_> {
    fn commit_inner(self: Box<Self>) -> Result<()> {
        block_in_place(|| tokio::runtime::Handle::current().block_on(self.inner.commit()))
            .context("Failed to commit transaction")
    }
}

impl DbConnection for PgTransaction<'_> {
    pg_query_methods!(|this| exec = &this.inner, cache = this.cache);
    pg_shared_methods!();
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
            "data::jsonb->>'title'"
        );
        assert_eq!(
            pg_json_each_source("col", "x"),
            "jsonb_array_elements_text(col) AS x"
        );
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
        assert_eq!(
            pg_build_upsert("t", &["id", "name"], "$1, $2", "id"),
            "INSERT INTO \"t\" (\"id\", \"name\") VALUES ($1, $2) \
             ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\""
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
