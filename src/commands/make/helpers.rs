//! Shared helpers for the `make` command — registry loading utilities.

use std::path::Path;

use crate::{config::CrapConfig, core::SharedRegistry, hooks, scaffold::ConditionFieldInfo};

use super::hook::load_field_infos_from_registry;

/// Check if localization is enabled in the config dir's crap.toml.
pub fn has_locales_enabled(config_dir: &Path) -> bool {
    CrapConfig::load(config_dir)
        .map(|cfg| cfg.locale.is_enabled())
        .unwrap_or(false)
}

/// Try to load the Lua registry once for reuse across make helpers.
pub fn try_load_registry(config_dir: &Path) -> Option<SharedRegistry> {
    let config_dir = config_dir.canonicalize().ok()?;
    let cfg = CrapConfig::load(&config_dir).ok()?;

    hooks::init_lua(&config_dir, &cfg).ok()
}

/// Try to load collection slugs from the config dir for interactive selection.
pub fn try_load_collection_slugs(config_dir: &Path) -> Option<Vec<String>> {
    let registry = try_load_registry(config_dir)?;
    let reg = registry.read().ok()?;
    let mut slugs: Vec<String> = reg.collections.keys().map(|s| s.to_string()).collect();

    slugs.sort();

    Some(slugs)
}

/// Try to load field names for a collection from the config dir.
pub fn try_load_field_names(config_dir: &Path, collection: &str) -> Option<Vec<String>> {
    let registry = try_load_registry(config_dir)?;
    let reg = registry.read().ok()?;
    let def = reg.get_collection(collection)?;

    Some(def.fields.iter().map(|f| f.name.clone()).collect())
}

/// Try to load field definitions (name + type + options) for condition hook scaffolding.
pub fn try_load_field_infos(
    config_dir: &Path,
    collection: &str,
) -> Option<Vec<ConditionFieldInfo>> {
    let registry = try_load_registry(config_dir)?;

    load_field_infos_from_registry(&Some(registry), collection)
}
