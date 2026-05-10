//! Database layer: backend-agnostic connection abstraction, SQLite +
//! Postgres backends, pool, migrations, and CRUD/find/populate queries.
//!
//! ## Submodule layout
//!
//! - `connection.rs` -- the [`DbConnection`] trait + [`BoxedConnection`]
//!   / [`BoxedTransaction`] type-erased wrappers. Backend-agnostic
//!   interface used by every higher layer.
//! - `pool.rs` -- [`DbPool`] (Arc'd `dyn PoolBackend`); `pool.get()` →
//!   [`BoxedConnection`].
//! - `backend/{sqlite,postgres}.rs` -- per-engine `PoolBackend` +
//!   connection impls, migrations runner, and engine-specific tuning.
//!   Gated by the `sqlite` / `postgres` cargo features.
//! - `migrate/` -- schema migration runner, collection sync, join-table
//!   provisioning, FTS table sync, ref-count backfill.
//! - `query/` -- read + write helpers shared across the service layer:
//!   `find` / `find_by_id` / `count`, `create` / `update` / `delete`,
//!   FTS upsert/delete, ref-count maintenance, populate (single +
//!   batched), filter resolution, jobs, and image-queue claim.
//! - `ops.rs` -- a thin compatibility shim used by tests; production
//!   callers reach the underlying `query::*` directly.
//! - `types.rs` -- the [`DbValue`] / [`DbRow`] data shapes returned by
//!   the connection trait.
//! - `document.rs` -- per-collection `Document` row shape produced by
//!   the read path.
//!
//! ## Conventions
//!
//! - Every `query::*` function takes `&dyn DbConnection`, so the same
//!   helper works against either a [`BoxedConnection`] or a
//!   [`BoxedTransaction`].
//! - Service-level callers open transactions; `query::*` never opens
//!   its own.
//! - Per-engine SQL differences route through the [`DbConnection`]
//!   trait (`placeholder()`, `now_expr()`, `kind()`, etc.) instead of
//!   `if conn.kind() == "sqlite"` branching at call sites.

pub mod backend;
pub mod connection;
pub mod document;
pub mod migrate;
pub mod ops;
pub mod pool;
pub mod query;
pub mod types;

pub use connection::{BoxedConnection, BoxedTransaction, DbConnection};
pub use pool::DbPool;
pub use query::{
    AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, LocaleMode,
    PaginationResult, SharedPopulateSingleflight, Singleflight,
};
pub use types::{DbRow, DbValue};

#[cfg(all(test, feature = "sqlite"))]
pub use backend::sqlite::InMemoryConn;
