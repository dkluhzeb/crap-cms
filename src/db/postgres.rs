//! PostgreSQL backend — connection, transaction, and pool implementation.
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

use crate::{config::CrapConfig, core::FieldType};

use super::{
    connection::{BoxedConnection, ConnectionInner, DbConnection, TransactionInner},
    pool::DbPool,
    types::{DbRow, DbValue},
};

// ── Shared trait methods (non-query) ─────────────────────────────────────

/// Methods that don't depend on the client type — implemented identically
/// for both `PgConnection` and `PgTransaction`.
macro_rules! pg_shared_methods {
    () => {
        fn placeholder(&self, n: usize) -> String {
            format!("${n}")
        }

        fn now_expr(&self) -> &'static str {
            "to_char(NOW(), 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        }

        fn greatest_expr(&self, a: &str, b: &str) -> String {
            format!("GREATEST({a}, {b})")
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
            match ft {
                FieldType::Number => "DOUBLE PRECISION",
                FieldType::Checkbox => "BIGINT",
                _ => "TEXT",
            }
        }

        fn date_offset_expr(&self, seconds: i64, param_pos: usize) -> (String, DbValue) {
            // Build the offset expression using make_interval() which takes
            // an integer (seconds), avoiding the TEXT→interval cast issue.
            // We pass seconds as an integer param, which tokio-postgres handles.
            let sql = format!(
                "to_char(NOW() + make_interval(secs => ${param_pos}), \
                 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
            );
            (sql, DbValue::Real(seconds as f64))
        }

        fn json_extract_expr(&self, column: &str, field: &str) -> String {
            format!("{column}::jsonb->>'{field}'")
        }

        fn json_each_source(&self, source: &str, alias: &str) -> String {
            format!("jsonb_array_elements_text({source}) AS {alias}")
        }

        fn build_insert_ignore(&self, table: &str, columns: &str, values: &str) -> String {
            format!("INSERT INTO \"{table}\" ({columns}) VALUES ({values}) ON CONFLICT DO NOTHING")
        }

        fn build_upsert(
            &self,
            table: &str,
            columns: &[&str],
            values: &str,
            key_col: &str,
        ) -> String {
            let cols = columns
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", ");
            let updates = columns
                .iter()
                .filter(|c| **c != key_col)
                .map(|c| format!("\"{}\" = EXCLUDED.\"{}\"", c, c))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO \"{table}\" ({cols}) VALUES ({values}) \
                 ON CONFLICT (\"{key_col}\") DO UPDATE SET {updates}"
            )
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
            ts.to_string()
        }
    };
}

// ── Statement-cached pool ────────────────────────────────────────────────

/// A pooled tokio_postgres `Client` plus a per-connection prepared-statement
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

/// Create a PostgreSQL connection pool from config.
pub fn create_pool(config: &CrapConfig) -> Result<DbPool> {
    let url = config
        .database
        .url
        .as_deref()
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

impl super::pool::PoolBackend for PgPoolBackend {
    fn get(&self) -> Result<BoxedConnection> {
        let obj = block_in_place(|| tokio::runtime::Handle::current().block_on(self.pool.get()))
            .map_err(|e| anyhow!("Failed to get Postgres connection: {}", e))?;

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
///   actual execute/query AND for prepare(). Both Client and Transaction
///   have prepare(); Statements are connection-bound and survive the
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
            Ok(count as usize)
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

/// Convert `DbValue` slice to tokio-postgres parameter boxes.
fn to_pg_params(params: &[DbValue]) -> Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> {
    params
        .iter()
        .map(|v| -> Box<dyn tokio_postgres::types::ToSql + Sync + Send> {
            match v {
                DbValue::Null => Box::new(None::<String>),
                // Always send as i64 (BIGINT). Postgres implicitly downcasts
                // BIGINT to INTEGER for column inserts/updates.
                DbValue::Integer(i) => Box::new(*i),
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
            Ok(Some(b)) => DbValue::Integer(if b { 1 } else { 0 }),
            _ => DbValue::Null,
        },
        Type::INT2 => match row.try_get::<_, Option<i16>>(idx) {
            Ok(Some(v)) => DbValue::Integer(v as i64),
            _ => DbValue::Null,
        },
        Type::INT4 => match row.try_get::<_, Option<i32>>(idx) {
            Ok(Some(v)) => DbValue::Integer(v as i64),
            _ => DbValue::Null,
        },
        Type::INT8 => match row.try_get::<_, Option<i64>>(idx) {
            Ok(Some(v)) => DbValue::Integer(v),
            _ => DbValue::Null,
        },
        Type::FLOAT4 => match row.try_get::<_, Option<f32>>(idx) {
            Ok(Some(v)) => DbValue::Real(v as f64),
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
