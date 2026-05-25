//! Per-engine backend implementations of [`crate::db::pool::PoolBackend`]
//! and [`crate::db::connection::DbConnection`]. Each submodule exposes a
//! `create_pool` factory called from [`crate::db::pool::create_pool`].

#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "sqlite")]
pub mod sqlite;
