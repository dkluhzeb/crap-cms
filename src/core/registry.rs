//! In-memory registry of collection and global definitions loaded from Lua.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use super::collection::{CollectionDefinition, GlobalDefinition};

/// Holds all collection and global definitions loaded at startup.
pub struct Registry {
    pub collections: HashMap<String, CollectionDefinition>,
    pub globals: HashMap<String, GlobalDefinition>,
}

/// Thread-safe shared reference to the registry.
pub type SharedRegistry = Arc<RwLock<Registry>>;

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// Create an empty registry with no collections or globals.
    pub fn new() -> Self {
        Self {
            collections: HashMap::new(),
            globals: HashMap::new(),
        }
    }

    /// Create a new registry wrapped in `Arc<RwLock<>>` for shared ownership.
    pub fn shared() -> SharedRegistry {
        Arc::new(RwLock::new(Self::new()))
    }

    /// Register a collection definition, keyed by slug. Overwrites any existing definition.
    pub fn register_collection(&mut self, def: CollectionDefinition) {
        tracing::debug!("Registering collection '{}'", def.slug);
        self.collections.insert(def.slug.clone(), def);
    }

    /// Register a global definition, keyed by slug. Overwrites any existing definition.
    pub fn register_global(&mut self, def: GlobalDefinition) {
        tracing::debug!("Registering global '{}'", def.slug);
        self.globals.insert(def.slug.clone(), def);
    }

    /// Look up a collection definition by slug.
    pub fn get_collection(&self, slug: &str) -> Option<&CollectionDefinition> {
        self.collections.get(slug)
    }

    /// Look up a global definition by slug.
    pub fn get_global(&self, slug: &str) -> Option<&GlobalDefinition> {
        self.globals.get(slug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::{CollectionLabels, CollectionAdmin, CollectionHooks, CollectionAccess};

    fn make_collection(slug: &str) -> CollectionDefinition {
        CollectionDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: Vec::new(),
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        }
    }

    #[test]
    fn register_and_get_collection() {
        let mut reg = Registry::new();
        assert!(reg.get_collection("posts").is_none());

        reg.register_collection(make_collection("posts"));
        assert!(reg.get_collection("posts").is_some());
        assert_eq!(reg.get_collection("posts").unwrap().slug, "posts");
    }

    #[test]
    fn register_overwrites_existing() {
        let mut reg = Registry::new();
        reg.register_collection(make_collection("posts"));
        reg.register_collection(make_collection("posts"));
        assert_eq!(reg.collections.len(), 1);
    }

    #[test]
    fn shared_registry_is_accessible() {
        let shared = Registry::shared();
        let mut reg = shared.write().unwrap();
        reg.register_collection(make_collection("pages"));
        assert_eq!(reg.collections.len(), 1);
    }

    fn make_global(slug: &str) -> GlobalDefinition {
        GlobalDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            fields: Vec::new(),
            hooks: CollectionHooks::default(),
            access: CollectionAccess::default(),
            live: None,
        }
    }

    #[test]
    fn register_and_get_global() {
        let mut reg = Registry::new();
        assert!(reg.get_global("settings").is_none());

        reg.register_global(make_global("settings"));
        assert!(reg.get_global("settings").is_some());
        assert_eq!(reg.get_global("settings").unwrap().slug, "settings");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let reg = Registry::new();
        assert!(reg.get_global("nonexistent").is_none());
    }
}
