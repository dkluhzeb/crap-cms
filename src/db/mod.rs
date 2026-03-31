//! Database layer: connection abstraction, SQLite backend, pool, migration, CRUD queries, and read wrappers.

pub mod connection;
pub mod document;
pub mod migrate;
pub mod ops;
pub mod pool;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod query;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod types;

pub use connection::{BoxedConnection, BoxedTransaction, DbConnection};
pub use pool::DbPool;
pub use query::{
    AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, LocaleMode,
};
pub use types::{DbRow, DbValue};

#[cfg(all(test, feature = "sqlite"))]
pub use sqlite::InMemoryConn;
