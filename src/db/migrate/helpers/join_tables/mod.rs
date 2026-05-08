//! Join table sync for has-many relationships, array fields, and blocks.

mod array;
mod blocks;
mod orchestrator;
mod relationship;

pub(in crate::db::migrate) use orchestrator::sync_join_tables;
