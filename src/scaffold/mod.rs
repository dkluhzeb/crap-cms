//! CLI scaffolding commands: init, make collection, make global, make hook, blueprints.
//!
//! Writes plain files to the config directory. No database, no hidden state.

pub mod blueprint;
pub mod collection;
pub mod global;
pub mod hook;
pub mod init;
pub mod job;
pub mod migration;
pub mod templates;

// Re-exports — preserve the flat `scaffold::*` API that callers use.
pub use self::blueprint::{
    blueprint_list, blueprint_remove, blueprint_save, blueprint_use, list_blueprint_names,
};
pub use self::collection::{CollectionOptions, VALID_FIELD_TYPES, make_collection};
pub use self::global::make_global;
pub use self::hook::{ConditionFieldInfo, HookType, MakeHookOptions, make_hook};
pub use self::init::{InitOptions, init};
pub use self::job::make_job;
pub use self::migration::make_migration;
pub use self::templates::{proto_export, templates_extract, templates_list};

// Re-export the shared title-case helper so submodules can call `super::to_title_case`.
pub(crate) use crate::core::field::to_title_case;

// Re-export from canonical location for backward compatibility.
pub use crate::db::query::validate_slug;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_title_case() {
        assert_eq!(to_title_case("posts"), "Posts");
        assert_eq!(to_title_case("site_settings"), "Site Settings");
        assert_eq!(to_title_case("my_cool_thing"), "My Cool Thing");
    }
}
