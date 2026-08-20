//! Field-level read/write access checks plus the recursive helpers that build
//! the per-field denied list — flat columns/groups and fields nested inside
//! array/blocks rows at any depth. `WriteHooks::check_access` and the read
//! post-processing apply the resulting `FieldDenial`s to strip denied fields.
//!
//! Split into: [`walk`] (pure `is_denied`-parameterized tree walkers),
//! [`check`] (Lua-evaluated per-field checks + denied-name collectors), and
//! [`strip`] (Lua data-aware in-place strip).

mod check;
mod strip;
mod walk;

pub(crate) use check::{
    check_field_read_access_with_lua, check_field_write_access_with_lua,
    collect_read_denied_with_lua, collect_write_denied_with_lua,
};
pub(crate) use strip::{
    ReadStripInput, WriteStripInput, strip_read_access_with_lua, strip_write_access_with_lua,
};
pub(crate) use walk::{
    collect_denials_flat, has_any_field_access, strip_access_data_aware,
    strip_read_access_data_aware,
};
