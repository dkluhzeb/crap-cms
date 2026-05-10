//! Layout of the scaffolded config directory. Single source of truth
//! for every `<config_dir>/<subdir>/...` path that scaffold generators
//! write to and that `init` creates upfront. A typo at one site can
//! no longer silently break the contract -- every generator + `init`
//! goes through these constants and helpers.

use std::path::{Path, PathBuf};

// == Top-level subdirectory names =======================================

pub(crate) const COLLECTIONS: &str = "collections";
pub(crate) const GLOBALS: &str = "globals";
pub(crate) const HOOKS: &str = "hooks";
pub(crate) const ACCESS: &str = "access";
pub(crate) const JOBS: &str = "jobs";
pub(crate) const PLUGINS: &str = "plugins";
pub(crate) const TEMPLATES: &str = "templates";
pub(crate) const STATIC: &str = "static";
pub(crate) const MIGRATIONS: &str = "migrations";
pub(crate) const TYPES: &str = "types";
pub(crate) const LUA: &str = "lua";

// == Nested subdirectory names ==========================================

pub(crate) const RICHTEXT_NODES: &str = "richtext_nodes";
pub(crate) const STYLES: &str = "styles";
pub(crate) const THEMES: &str = "themes";
pub(crate) const COMPONENTS: &str = "components";
pub(crate) const FIELDS: &str = "fields";
pub(crate) const PAGES: &str = "pages";
pub(crate) const SLOTS: &str = "slots";

/// Top-level subdirectories created by `crap-cms init`. Generators that
/// write into these paths assume `init` already laid them out.
pub(crate) const INIT_SUBDIRS: &[&str] = &[
    COLLECTIONS,
    GLOBALS,
    HOOKS,
    ACCESS,
    JOBS,
    PLUGINS,
    TEMPLATES,
    STATIC,
    MIGRATIONS,
    TYPES,
];

// == Path builders ======================================================

pub(crate) fn collections_dir(root: &Path) -> PathBuf {
    root.join(COLLECTIONS)
}

pub(crate) fn globals_dir(root: &Path) -> PathBuf {
    root.join(GLOBALS)
}

pub(crate) fn jobs_dir(root: &Path) -> PathBuf {
    root.join(JOBS)
}

pub(crate) fn plugins_dir(root: &Path) -> PathBuf {
    root.join(PLUGINS)
}

pub(crate) fn access_dir(root: &Path) -> PathBuf {
    root.join(ACCESS)
}

pub(crate) fn migrations_dir(root: &Path) -> PathBuf {
    root.join(MIGRATIONS)
}

pub(crate) fn collection_hooks_dir(root: &Path, collection: &str) -> PathBuf {
    root.join(HOOKS).join(collection)
}

pub(crate) fn templates_pages_dir(root: &Path) -> PathBuf {
    root.join(TEMPLATES).join(PAGES)
}

pub(crate) fn templates_fields_dir(root: &Path) -> PathBuf {
    root.join(TEMPLATES).join(FIELDS)
}

pub(crate) fn templates_slot_dir(root: &Path, slot: &str) -> PathBuf {
    root.join(TEMPLATES).join(SLOTS).join(slot)
}

pub(crate) fn static_components_dir(root: &Path) -> PathBuf {
    root.join(STATIC).join(COMPONENTS)
}

pub(crate) fn static_themes_dir(root: &Path) -> PathBuf {
    root.join(STATIC).join(STYLES).join(THEMES)
}

pub(crate) fn richtext_nodes_dir(root: &Path) -> PathBuf {
    root.join(LUA).join(RICHTEXT_NODES)
}
