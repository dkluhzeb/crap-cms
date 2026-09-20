//! Global table sync: create and alter global tables from Lua definitions.

mod defaults;
mod sync;

pub(super) use sync::sync_global_table;
