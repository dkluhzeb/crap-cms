//! Database layer: connection abstraction, SQLite backend, pool, migration, CRUD queries, and read wrappers.

pub mod connection;
pub mod document;
pub mod migrate;
pub mod ops;
pub mod pool;
pub mod query;
pub mod sqlite;
pub mod types;

pub use connection::{BoxedConnection, BoxedTransaction, DbConnection};
pub use pool::DbPool;
pub use query::{
    AccessResult, Filter, FilterClause, FilterOp, FindQuery, LocaleContext, LocaleMode,
};
pub use types::{DbRow, DbValue};

#[cfg(test)]
pub use sqlite::InMemoryConn;
