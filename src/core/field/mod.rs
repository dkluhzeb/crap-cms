//! Field types and definitions. Each field maps to a column (or join table) in SQLite.

mod admin;
mod block_definition;
mod definition;
mod field_type;
mod localized_string;
mod relationship;
mod select_option;

pub use admin::{FieldAdmin, FieldAdminBuilder, validate_template_name};
pub use block_definition::{BlockDefinition, FieldTab};
pub use definition::{
    FieldAccess, FieldDefinition, FieldDefinitionBuilder, FieldHooks, McpFieldConfig,
    flatten_array_sub_fields, to_title_case,
};
pub use field_type::FieldType;
pub use localized_string::LocalizedString;
pub use relationship::{JoinConfig, RelationshipConfig};
pub use select_option::SelectOption;
