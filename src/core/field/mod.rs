//! Field types and definitions. Each field maps to a column (or join table) in SQLite.

mod block_definition;
mod field_admin;
mod field_admin_builder;
mod field_definition;
mod field_definition_builder;
mod field_type;
mod localized_string;
mod relationship;
mod select_option;

pub use block_definition::{BlockDefinition, FieldTab};
pub use field_admin::{FieldAdmin, validate_template_name};
pub use field_admin_builder::FieldAdminBuilder;
pub use field_definition::{
    FieldAccess, FieldDefinition, FieldHooks, McpFieldConfig, flatten_array_sub_fields,
    to_title_case,
};
pub use field_definition_builder::FieldDefinitionBuilder;
pub use field_type::FieldType;
pub use localized_string::LocalizedString;
pub use relationship::{JoinConfig, RelationshipConfig};
pub use select_option::SelectOption;
