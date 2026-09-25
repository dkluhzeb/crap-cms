//! The keys a field table accepts: the common set plus its type's own.

use anyhow::Result;
use mlua::Table;

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
pub(super) fn validate_field_keys(field_tbl: &Table, field_type: &FieldType) -> Result<()> {
    let mut allowed: Vec<&str> = COMMON_FIELD_KEYS.to_vec();
    allowed.extend_from_slice(type_specific_field_keys(field_type));

    deny_unknown_keys(
        field_tbl,
        &format!("{} field", field_type.as_str()),
        &allowed,
    )
}
