//! A field's `access` and `hooks` sub-tables.

use anyhow::{Result, bail};
use mlua::{Table, Value};

use crate::{
    core::{FieldAccess, FieldHooks, FieldType},
    hooks::lua_api::parse::helpers::{
        deny_unknown_keys, get_optional_hook_ref, parse_hook_ref_list,
    },
};

/// The rules a field `access` sub-table accepts.
const FIELD_ACCESS_KEYS: &[&str] = &["read", "create", "update"];

/// The only `access` rule and lifecycle hook a join accepts: a join is never
/// written (no write carries it), so a write-side rule or hook can never run
/// on it — only reads see its value.
const JOIN_ACCESS_KEYS: &[&str] = &["read"];
const JOIN_HOOK_KEYS: &[&str] = &["after_read"];

/// Refuse a `sub`-table key a join accepts on other fields but can never
/// honor (`access.create`, `hooks.before_change`, …). Presence is what counts.
fn deny_write_side_keys_on_join(
    tbl: &Table,
    sub: &str,
    keys: (&[&str], &[&str]),
    field: (&FieldType, &str),
) -> Result<()> {
    let (known, accepted) = keys;
    let (field_type, name) = field;

    if *field_type != FieldType::Join {
        return Ok(());
    }

    for key in known.iter().filter(|key| !accepted.contains(*key)) {
        if matches!(tbl.get::<Value>(*key)?, Value::Nil) {
            continue;
        }

        bail!(
            "join field '{name}': '{sub}.{key}' has no effect on a join — a join is \
             virtual and never written, so only its reads apply ({})",
            accepted.join(", ")
        );
    }

    Ok(())
}

/// Parse a field's `access` sub-table. Each rule is a string hook reference;
/// a present-but-non-string value is a hard error rather than being silently
/// dropped (parity with collection/global `access`). A join accepts only
/// `read`.
///
/// # Errors
///
/// Returns an error for an unknown rule, a write rule on a join, or a rule
/// present but not a string.
pub(super) fn parse_field_access(
    access_tbl: &Table,
    field_type: &FieldType,
    name: &str,
) -> Result<FieldAccess> {
    deny_unknown_keys(access_tbl, "field access", FIELD_ACCESS_KEYS)?;
    deny_write_side_keys_on_join(
        access_tbl,
        "access",
        (FIELD_ACCESS_KEYS, JOIN_ACCESS_KEYS),
        (field_type, name),
    )?;

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

/// Parse a field's `hooks` sub-table. A join accepts only `after_read`.
pub(super) fn parse_field_hooks(
    hooks_tbl: &Table,
    field_type: &FieldType,
    name: &str,
) -> Result<FieldHooks> {
    deny_unknown_keys(hooks_tbl, "field hooks", FIELD_HOOK_KEYS)?;
    deny_write_side_keys_on_join(
        hooks_tbl,
        "hooks",
        (FIELD_HOOK_KEYS, JOIN_HOOK_KEYS),
        (field_type, name),
    )?;

    Ok(FieldHooks {
        before_validate: parse_hook_ref_list(hooks_tbl, "before_validate")?,
        before_change: parse_hook_ref_list(hooks_tbl, "before_change")?,
        after_change: parse_hook_ref_list(hooks_tbl, "after_change")?,
        after_read: parse_hook_ref_list(hooks_tbl, "after_read")?,
    })
}
