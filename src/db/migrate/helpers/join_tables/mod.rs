//! Join table sync for has-many relationships, array fields, and blocks —
//! including the `parent_id` index of every array / blocks row table.

mod array;
mod blocks;
mod orchestrator;
mod parent_index;
mod relationship;

pub(in crate::db::migrate) use orchestrator::sync_join_tables;
