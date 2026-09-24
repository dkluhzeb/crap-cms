//! The per-connection prepared-statement cache behind the Postgres backend:
//! the pooled client that carries it, the deadpool manager that creates and
//! recycles such clients, and the one call path every query takes through
//! the cache.

use std::collections::HashMap;

use deadpool::managed::{self, Metrics, RecycleError, RecycleResult};
use parking_lot::Mutex;
use tokio_postgres::{Client, Error as PgError, NoTls, Statement, error::SqlState};
use tracing::{error, warn};

/// Result of a raw driver call, before it is wrapped in an `anyhow` context.
pub(super) type PgResult<T> = std::result::Result<T, PgError>;

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
    pub(super) client: Client,
    pub(super) cache: Mutex<HashMap<String, Statement>>,
}

/// Custom deadpool Manager that produces `CachedClient` instances. We can't
/// use `deadpool_postgres::Manager` because its pooled `Type` is the bare
/// `tokio_postgres::Client` — there's no place to attach the cache.
pub struct CachedManager {
    pub(super) config: tokio_postgres::Config,
}

impl managed::Manager for CachedManager {
    type Type = CachedClient;
    type Error = PgError;

    async fn create(&self) -> PgResult<CachedClient> {
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

        // Only warnings and errors reach the client. The schema sync runs
        // `CREATE … IF NOT EXISTS` on every boot, and each one that finds its
        // object answers with a NOTICE ("relation … already exists,
        // skipping") that the connection driver logs at INFO — a page of noise
        // per start, drowning the lines an operator reads. Any user may set
        // this parameter.
        client
            .batch_execute("SET client_min_messages = 'warning'")
            .await?;

        // Error classification reads SQLSTATE, but the one condition Postgres
        // gives no code of its own — a cached plan invalidated by a schema
        // change — is identifiable only by its message, and message text is
        // translated according to `lc_messages`. Pinning the session to `C`
        // keeps that text English on a localized server. The parameter can be
        // reserved to superusers, so a refusal is logged and tolerated: the
        // SQLSTATE half of every classification still holds without it.
        if let Err(e) = client.batch_execute("SET lc_messages = 'C'").await {
            warn!("Could not pin lc_messages to 'C' on a Postgres connection: {e}");
        }

        Ok(CachedClient {
            client,
            cache: Mutex::new(HashMap::new()),
        })
    }

    async fn recycle(&self, client: &mut CachedClient, _: &Metrics) -> RecycleResult<PgError> {
        // A client whose connection has died (server restart, network drop, an
        // administrative terminate) must leave the pool: nothing revives it,
        // and every later checkout of it fails on a dead socket for the life of
        // the process. Its statement cache goes with it; the replacement
        // re-prepares what it needs.
        if client.client.is_closed() {
            return Err(RecycleError::message("connection closed"));
        }

        // Otherwise a no-op: cache + connection state are preserved across
        // checkouts. No `DISCARD ALL` — it would reset the session parameters
        // set at creation and throw away the prepared statements this cache
        // exists to keep, and we hold no other session-local state (no temp
        // tables, no advisory locks).
        Ok(())
    }
}

pub(super) type CachedPool = managed::Pool<CachedManager>;
pub(super) type CachedObject = managed::Object<CachedManager>;

/// The connection-wide statement cache one query runs against, and whether
/// the query runs inside a transaction.
///
/// A named prepared statement lives for the session — a rollback discards the
/// transaction's portals, not its statements — so a statement first prepared
/// inside a transaction is cached like any other. What a transaction changes
/// is the recovery from a stale plan: the failure has already aborted the
/// transaction, so a re-prepare there could only report "current transaction
/// is aborted" and hide the cause. Inside a transaction the statement is
/// evicted and the original error returned; the next transaction re-prepares.
pub(super) struct StmtCache<'a> {
    shared: &'a Mutex<HashMap<String, Statement>>,
    in_transaction: bool,
}

impl<'a> StmtCache<'a> {
    /// The cache of an autocommit connection.
    pub(super) fn connection(shared: &'a Mutex<HashMap<String, Statement>>) -> Self {
        Self {
            shared,
            in_transaction: false,
        }
    }

    /// The cache as seen from inside a transaction.
    pub(super) fn in_transaction(shared: &'a Mutex<HashMap<String, Statement>>) -> Self {
        Self {
            shared,
            in_transaction: true,
        }
    }

    fn get(&self, sql: &str) -> Option<Statement> {
        self.shared.lock().get(sql).cloned()
    }

    fn insert(&self, sql: &str, stmt: &Statement) {
        self.shared.lock().insert(sql.to_string(), stmt.clone());
    }

    fn remove(&self, sql: &str) {
        self.shared.lock().remove(sql);
    }

    /// Whether a stale statement may be re-prepared and the call retried —
    /// only in autocommit, where the failure aborted nothing.
    fn may_retry(&self) -> bool {
        !self.in_transaction
    }
}

/// Prepare a statement (cache lookup first), then return the cached
/// `Statement` ready for `client.execute(&stmt, &params)`. The `prepare`
/// callable is supplied by the caller so this works against either a
/// `Client` or a `Transaction` (both expose `prepare(&str)`); the caller
/// closes over `sql` so the borrow stays valid for the future's lifetime.
async fn cached_prepare<F, Fut>(cache: &StmtCache<'_>, sql: &str, prepare: F) -> PgResult<Statement>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = PgResult<Statement>>,
{
    if let Some(stmt) = cache.get(sql) {
        return Ok(stmt);
    }

    let stmt = prepare().await?;
    cache.insert(sql, &stmt);

    Ok(stmt)
}

/// Whether `e` says the cached `Statement` no longer names anything the server
/// will run — the one failure class re-preparing the SQL fixes.
fn is_stale_statement(e: &PgError) -> bool {
    let Some(code) = e.code() else {
        return false;
    };

    // 26000: the server has no statement under that name — something
    // discarded it out of band (a `DEALLOCATE`, a pooler's `DISCARD ALL`).
    if *code == SqlState::UNDEFINED_PSTATEMENT {
        return true;
    }

    // 0A000 covers several unrelated "not supported" conditions; only the
    // stale-plan one is retryable, and Postgres names it in the message —
    // kept English by the `lc_messages` pin taken at connection creation.
    *code == SqlState::FEATURE_NOT_SUPPORTED
        && e.as_db_error()
            .is_some_and(|db| db.message().contains("cached plan"))
}

/// Run one statement through the cache, re-preparing once when the cached
/// `Statement` has gone stale.
///
/// A concurrent `ALTER TABLE` — another node's schema sync, a migration run
/// against a live server — invalidates the plan behind a cached `Statement`,
/// and without the eviction that one SQL string then fails for the life of
/// the connection. `run` is called a second time only after a failure that
/// changed nothing, so the retry cannot duplicate a write; inside a
/// transaction there is no retry (see [`StmtCache`]).
pub(super) async fn cached_stmt_call<P, PFut, R, RFut, T>(
    cache: &StmtCache<'_>,
    sql: &str,
    prepare: P,
    run: R,
) -> PgResult<T>
where
    P: Fn() -> PFut,
    PFut: std::future::Future<Output = PgResult<Statement>>,
    R: Fn(Statement) -> RFut,
    RFut: std::future::Future<Output = PgResult<T>>,
{
    let stmt = cached_prepare(cache, sql, &prepare).await?;

    let err = match run(stmt).await {
        Ok(value) => return Ok(value),
        Err(e) => e,
    };

    if !is_stale_statement(&err) {
        return Err(err);
    }

    cache.remove(sql);

    if !cache.may_retry() {
        return Err(err);
    }

    let stmt = cached_prepare(cache, sql, &prepare).await?;

    run(stmt).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stale plan inside a transaction has already aborted it; a retry
    /// there would only surface "current transaction is aborted".
    #[test]
    fn a_stale_statement_is_retried_only_outside_a_transaction() {
        let shared = Mutex::new(HashMap::new());

        assert!(StmtCache::connection(&shared).may_retry());
        assert!(!StmtCache::in_transaction(&shared).may_retry());
    }
}
