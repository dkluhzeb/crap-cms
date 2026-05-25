//! CLI scaffolding commands: `init`, the `make <thing>` family, blueprints,
//! and template extraction. Each subcommand writes plain files to the
//! config directory -- no database access, no hidden state.
//!
//! ## Submodule layout
//!
//! - `init/` -- `crap-cms init`: project skeleton.
//! - `make/`-style siblings (one per `make` subcommand): `collection/`,
//!   `global/`, `hook/`, `job/`, `field/`, `component/`, `theme/`,
//!   `page/`, `slot/`, `node/`, `migration/`.
//! - `blueprint/` -- save / list / use / remove project blueprints.
//! - `templates.rs` -- `templates list` / `templates extract` for the
//!   embedded admin templates + static assets.
//! - `wizard.rs` -- interactive field-definition prompts shared by
//!   collection/global make.
//! - `render.rs` -- Handlebars registry that compiles in every
//!   `<sub>/templates/*.{hbs,tpl}` file via `include_str!` and renders
//!   them by name. Generators serialize a small context struct and call
//!   `render::render("<name>", &ctx)?`.
//! - `source_header.rs` -- version-tagged "do not edit" header inserted
//!   at the top of extracted templates.
//!
//! ## Conventions
//!
//! - No inline raw-string templates. Anything multi-line + variable-
//!   interpolated lives in `<sub>/templates/*.hbs` and is rendered via
//!   `render::render`.
//! - Slug validation: `validate_slug` for SQL/Lua identifiers
//!   (collections, fields, ...); `validate_template_slug` for file-path
//!   slugs that allow hyphens (pages, slots, themes).
//! - Submodules are `pub(crate) mod` -- the public API is the flat
//!   `scaffold::*` re-export block below.

pub(crate) mod blueprint;
pub(crate) mod collection;
pub(crate) mod component;
pub(crate) mod field;
pub(crate) mod global;
pub(crate) mod guards;
pub(crate) mod hook;
pub(crate) mod init;
pub(crate) mod job;
pub(crate) mod migration;
pub(crate) mod node;
pub(crate) mod page;
pub(crate) mod paths;
pub(crate) mod render;
pub(crate) mod slot;
pub(crate) mod source_header;
pub(crate) mod templates;
pub(crate) mod theme;
pub(crate) mod wizard;

// Re-exports -- preserve the flat `scaffold::*` API that callers use.
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
pub use self::templates::{
    TemplatesExtractParams, proto_export, templates_extract, templates_list,
};

// Crate-internal: the embedded admin-template / static-asset trees.
// Reached from `commands::templates::*` for `templates list/extract`.
// Not part of the scaffold public API -- they're compiled-in data, not
// callable functions.
pub(crate) use self::templates::{EMBEDDED_STATIC, EMBEDDED_TEMPLATES};
pub use self::theme::{MakeThemeOptions, make_theme};
pub use self::wizard::interactive_field_wizard;

// Re-export the shared title-case helper so submodules can call `super::to_title_case`.
pub(crate) use crate::core::to_title_case;

// Re-export from canonical location for backward compatibility.
//
// `validate_slug` -- strict, used for slugs that map to SQL or Lua
// identifiers (collections, globals, fields, jobs, nodes).
//
// `validate_template_slug` -- looser, allows hyphens, used for slugs
// that map to file paths or URL segments (custom pages, slots,
// themes).
pub use crate::db::query::{validate_slug, validate_template_slug};
