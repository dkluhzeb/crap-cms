//! CLI scaffolding commands: init, make collection, make global, make hook, blueprints.
//!
//! Writes plain files to the config directory. No database, no hidden state.

pub mod init;
pub mod collection;
pub mod global;
pub mod hook;
pub mod job;
pub mod migration;
pub mod blueprint;
pub mod templates;

// Re-exports — preserve the flat `scaffold::*` API that callers use.
pub use self::init::{init, InitOptions};
pub use self::collection::{make_collection, VALID_FIELD_TYPES};
pub use self::global::make_global;
pub use self::hook::{make_hook, HookType, MakeHookOptions, ConditionFieldInfo};
pub use self::job::make_job;
pub use self::migration::make_migration;
pub use self::blueprint::{
    blueprint_save, blueprint_use, blueprint_list, blueprint_remove, list_blueprint_names,
};
pub use self::templates::{templates_list, templates_extract, proto_export};

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
