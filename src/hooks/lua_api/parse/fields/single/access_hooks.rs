//! A field's `access` and `hooks` sub-tables.

use anyhow::Result;
use mlua::Table;

use crate::{
    core::{FieldAccess, FieldHooks},
    hooks::lua_api::parse::helpers::{
        deny_unknown_keys, get_optional_hook_ref, parse_hook_ref_list,
    },
};

/// Parse a field's `access` sub-table. Each rule is a string hook reference;
/// a present-but-non-string value is a hard error rather than being silently
/// dropped (parity with collection/global `access`).
///
/// # Errors
///
/// Returns an error if `read`/`create`/`update` is present but not a string.
pub(super) fn parse_field_access(access_tbl: &Table) -> Result<FieldAccess> {
    Ok(FieldAccess {
        read: get_optional_hook_ref(access_tbl, "read", "field access")?,
        create: get_optional_hook_ref(access_tbl, "create", "field access")?,
        update: get_optional_hook_ref(access_tbl, "update", "field access")?,
    })
}

/// Lifecycle hook keys accepted on a field `hooks` sub-table.
/// `pub(crate)` so the `make hook` scaffold pins its position list to this
/// (the runtime source of truth) in a parity test.
pub(crate) const FIELD_HOOK_KEYS: &[&str] = &[
    "before_validate",
    "before_change",
    "after_change",
    "after_read",
];

pub(super) fn parse_field_hooks(hooks_tbl: &Table) -> Result<FieldHooks> {
    deny_unknown_keys(hooks_tbl, "field hooks", FIELD_HOOK_KEYS)?;

    Ok(FieldHooks {
        before_validate: parse_hook_ref_list(hooks_tbl, "before_validate")?,
        before_change: parse_hook_ref_list(hooks_tbl, "before_change")?,
        after_change: parse_hook_ref_list(hooks_tbl, "after_change")?,
        after_read: parse_hook_ref_list(hooks_tbl, "after_read")?,
    })
}
