//! CLI scaffolding commands: init, make collection, make global, make hook, blueprints.
//!
//! Writes plain files to the config directory. No database, no hidden state.

pub mod blueprint;
pub mod collection;
pub mod component;
pub mod field;
pub mod global;
pub mod hook;
pub mod init;
pub mod job;
pub mod migration;
pub mod node;
pub mod page;
pub(crate) mod render;
pub mod slot;
pub mod source_header;
pub mod templates;
pub mod theme;
pub mod wizard;

// Re-exports — preserve the flat `scaffold::*` API that callers use.
pub use self::blueprint::{
    blueprint_list, blueprint_remove, blueprint_save, blueprint_use, list_blueprint_names,
};
pub use self::collection::{
    BlockStub, CollectionOptions, FieldStub, TabStub, VALID_FIELD_TYPES, make_collection,
    parse_fields_shorthand,
};
pub use self::component::{MakeComponentOptions, make_component};
pub use self::field::{MakeFieldOptions, make_field};
pub use self::global::make_global;
pub use self::hook::{ConditionFieldInfo, HookType, MakeHookOptions, make_hook};
pub use self::init::{InitOptions, init};
pub use self::job::{MakeJobOptions, make_job};
pub use self::migration::make_migration;
pub use self::node::{MakeNodeOptions, make_node};
pub use self::page::{MakePageOptions, make_page};
pub use self::slot::{MakeSlotOptions, make_slot};
pub use self::templates::{proto_export, templates_extract, templates_list};
pub use self::theme::{MakeThemeOptions, make_theme};
pub use self::wizard::interactive_field_wizard;

// Re-export the shared title-case helper so submodules can call `super::to_title_case`.
pub(crate) use crate::core::field::to_title_case;

// Re-export from canonical location for backward compatibility.
pub use crate::db::query::validate_slug;
