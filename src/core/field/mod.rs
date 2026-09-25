//! Field types and definitions. Each field maps to a column (or join table) in `SQLite`.

mod admin;
mod block_definition;
mod companion;
mod definition;
mod field_type;
mod localized_string;
mod references;
mod relationship;
mod select_option;
mod storage;

pub use admin::{
    FieldAdmin, FieldAdminBuilder, FieldAdminLabels, FieldWidth, validate_template_name,
};
pub use block_definition::{BLOCK_TYPE_KEY, BlockDefinition, FieldTab};
pub(crate) use companion::{Companion, LANG_SUFFIX, TZ_SUFFIX};
pub use definition::{
    FieldAccess, FieldDefinition, FieldDefinitionBuilder, FieldHookFn, FieldHooks, McpFieldConfig,
    PickerAppearance, RequiredLocales, ValidateFunction, to_title_case,
};
pub use field_type::FieldType;
pub use localized_string::{
    LocalizedString, current_label_locale, default_label_locale, in_label_locale,
    set_default_label_locale, spawn_blocking_in_label_locale, with_label_locale,
};
pub use references::reference_items;
pub use relationship::{DEFAULT_JOIN_LIMIT, JoinConfig, RelationshipConfig};
pub use select_option::SelectOption;

// The field-tree walkers live in `core::walk`; re-export `flatten_array_sub_fields`
// here so existing `core::field::flatten_array_sub_fields` call sites resolve.
pub use crate::core::walk::flatten_array_sub_fields;
