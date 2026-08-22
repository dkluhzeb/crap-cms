//! Field types and definitions. Each field maps to a column (or join table) in `SQLite`.

mod admin;
mod block_definition;
mod definition;
mod field_type;
mod localized_string;
mod relationship;
mod select_option;

pub use admin::{
    FieldAdmin, FieldAdminBuilder, FieldAdminLabels, FieldWidth, validate_template_name,
};
pub use block_definition::{BLOCK_TYPE_KEY, BlockDefinition, FieldTab};
pub use definition::{
    FieldAccess, FieldDefinition, FieldDefinitionBuilder, FieldHookFn, FieldHooks, McpFieldConfig,
    PickerAppearance, RequiredLocales, ValidateFunction, to_title_case,
};
pub use field_type::FieldType;
pub use localized_string::LocalizedString;
pub use relationship::{JoinConfig, RelationshipConfig};
pub use select_option::SelectOption;

// The field-tree walkers live in `core::walk`; re-export `flatten_array_sub_fields`
// here so existing `core::field::flatten_array_sub_fields` call sites resolve.
pub use crate::core::walk::flatten_array_sub_fields;
