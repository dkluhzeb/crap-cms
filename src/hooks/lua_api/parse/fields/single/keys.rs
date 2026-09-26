//! The keys a field table accepts: the common set plus its type's own.

use anyhow::{Result, bail};
use mlua::{Table, Value};

use crate::{core::FieldType, hooks::lua_api::parse::helpers::deny_unknown_keys};

/// Keys accepted on every field table regardless of type. Type-specific keys
/// are appended by [`type_specific_field_keys`]. Unknown keys are rejected
/// (parity with the strict jobs config), so a typo like `requird` or a
/// misplaced key like `options` on a text field fails loudly at load time.
const COMMON_FIELD_KEYS: &[&str] = &[
    "name",
    "type",
    "required",
    "required_when",
    "unique",
    "index",
    "localized",
    "required_locales",
    "hidden",
    "validate",
    "default_value",
    "admin",
    "hooks",
    "access",
    "mcp",
];

/// The common keys a transparent layout wrapper (row / collapsible / tabs)
/// accepts. Every other common key — `access`, `hidden`, `hooks`, `required`,
/// `unique`, `index`, `localized`, `validate`, `default_value`, `mcp`, … —
/// describes a stored value, and a wrapper has none: its children sit at the
/// wrapper's own level and every walker (read/write access strip, hidden
/// strip, validation, FTS, MCP schema) looks through it. Such a key would be a
/// silent no-op — for `access` / `hidden` one that leaves the wrapped fields
/// open to everyone — so it is refused at load.
const WRAPPER_FIELD_KEYS: &[&str] = &["name", "type", "admin"];

/// The common keys a join accepts. A join is a virtual, read-only list of the
/// documents referencing this one: it has no stored value, so nothing
/// validates, defaults, indexes, localizes or writes it — every other common
/// key (`required`, `required_when`, `unique`, `index`, `localized`,
/// `required_locales`, `validate`, `default_value`, `mcp`) would be a silent
/// no-op, and is refused at load. `access` and `hooks` are narrowed to their
/// read-side keys by their own parsers.
const JOIN_FIELD_KEYS: &[&str] = &["name", "type", "admin", "hooks", "hidden", "access"];

/// Keys valid only on specific field types, appended to [`COMMON_FIELD_KEYS`].
/// `min_rows`/`max_rows` bound the count of multi-value fields; the string
/// length bounds apply to text-backed types; `relation_to` is the legacy flat
/// relationship syntax kept for back-compat.
fn type_specific_field_keys(field_type: &FieldType) -> &'static [&'static str] {
    match field_type {
        FieldType::Text => &[
            "has_many",
            "min_length",
            "max_length",
            "min_rows",
            "max_rows",
        ],
        FieldType::Number => &["has_many", "min", "max", "integer", "min_rows", "max_rows"],
        FieldType::Textarea | FieldType::Richtext | FieldType::Email | FieldType::Code => {
            &["min_length", "max_length"]
        }
        FieldType::Select | FieldType::Radio => &["options", "has_many", "min_rows", "max_rows"],
        FieldType::Checkbox | FieldType::Json => &[],
        FieldType::Date => &[
            "min_date",
            "max_date",
            "picker_appearance",
            "timezone",
            "default_timezone",
        ],
        FieldType::Relationship | FieldType::Upload => &[
            "relationship",
            "relation_to",
            "has_many",
            "min_rows",
            "max_rows",
        ],
        FieldType::Array => &["fields", "min_rows", "max_rows"],
        FieldType::Group | FieldType::Row | FieldType::Collapsible => &["fields"],
        FieldType::Tabs => &["tabs"],
        FieldType::Blocks => &["blocks", "min_rows", "max_rows"],
        FieldType::Join => &["collection", "on", "limit"],
    }
}

/// Reject any key on a field table that is not valid for its type.
pub(super) fn validate_field_keys(
    field_tbl: &Table,
    field_type: &FieldType,
    name: &str,
) -> Result<()> {
    let common = common_field_keys(field_type);
    deny_inert_common_keys(field_tbl, field_type, name, common)?;

    let mut allowed: Vec<&str> = common.to_vec();
    allowed.extend_from_slice(type_specific_field_keys(field_type));

    deny_unknown_keys(
        field_tbl,
        &format!("{} field", field_type.as_str()),
        &allowed,
    )
}

/// The common keys `field_type` accepts: a layout wrapper and a join accept
/// only the ones that can have an effect on them.
fn common_field_keys(field_type: &FieldType) -> &'static [&'static str] {
    if field_type.is_layout_wrapper() {
        return WRAPPER_FIELD_KEYS;
    }

    if *field_type == FieldType::Join {
        return JOIN_FIELD_KEYS;
    }

    COMMON_FIELD_KEYS
}

/// Why a common key outside `field_type`'s accepted set can have no effect on
/// it, and where it belongs instead.
fn inert_key_reason(field_type: &FieldType, key: &str) -> String {
    if *field_type == FieldType::Join {
        return "has no effect on a join — a join is a virtual, read-only list of the \
                documents whose 'on' field references this one; it has no stored value \
                to validate, default, index, localize or write"
            .to_string();
    }

    format!(
        "has no effect on a layout wrapper (row/collapsible/tabs) — a wrapper has no \
         value of its own and its fields sit at the wrapper's level; set '{key}' on each \
         child field, or use a group field to scope the children under one value"
    )
}

/// Refuse a common key that `field_type` does not accept, naming why and where
/// it belongs instead. Presence is what counts (even `hidden = false` on a
/// wrapper, or `required = false` on a join): the key can never have an effect
/// there, and accepting its falsy form would suggest the truthy one works.
fn deny_inert_common_keys(
    field_tbl: &Table,
    field_type: &FieldType,
    name: &str,
    accepted: &[&str],
) -> Result<()> {
    let inert = COMMON_FIELD_KEYS
        .iter()
        .filter(|key| !accepted.contains(*key));

    for key in inert {
        if matches!(field_tbl.get::<Value>(*key)?, Value::Nil) {
            continue;
        }

        bail!(
            "{} field '{name}': '{key}' {}",
            field_type.as_str(),
            inert_key_reason(field_type, key)
        );
    }

    Ok(())
}
