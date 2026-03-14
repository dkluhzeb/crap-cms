//! In-memory registry of collection and global definitions loaded from Lua.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use crate::core::{
    CollectionDefinition, Slug, collection::GlobalDefinition, job::JobDefinition,
    richtext::RichtextNodeDef,
};

/// Holds all collection, global, and job definitions loaded at startup.
#[derive(Clone)]
pub struct Registry {
    pub collections: HashMap<Slug, CollectionDefinition>,
    pub globals: HashMap<Slug, GlobalDefinition>,
    pub jobs: HashMap<Slug, JobDefinition>,
    pub richtext_nodes: HashMap<String, RichtextNodeDef>,
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
            jobs: HashMap::new(),
            richtext_nodes: HashMap::new(),
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

    /// Register a job definition, keyed by slug. Overwrites any existing definition.
    pub fn register_job(&mut self, def: JobDefinition) {
        tracing::debug!("Registering job '{}'", def.slug);
        self.jobs.insert(def.slug.clone(), def);
    }

    /// Look up a job definition by slug.
    pub fn get_job(&self, slug: &str) -> Option<&JobDefinition> {
        self.jobs.get(slug)
    }

    /// Register a custom richtext node definition, keyed by name.
    pub fn register_richtext_node(&mut self, def: RichtextNodeDef) {
        tracing::debug!("Registering richtext node '{}'", def.name);
        self.richtext_nodes.insert(def.name.clone(), def);
    }

    /// Look up a custom richtext node definition by name.
    pub fn get_richtext_node(&self, name: &str) -> Option<&RichtextNodeDef> {
        self.richtext_nodes.get(name)
    }

    /// Create a read-only `Arc<Registry>` snapshot from a `SharedRegistry`.
    ///
    /// Call once after startup (after all `define()` writes) and pass the snapshot
    /// to hot-path consumers (admin UI, gRPC API) that only read the registry.
    pub fn snapshot(shared: &SharedRegistry) -> Arc<Registry> {
        let reg = shared
            .read()
            .expect("Registry lock poisoned during snapshot");
        Arc::new(reg.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::richtext::{NodeAttr, NodeAttrType, RichtextNodeDef};

    fn make_collection(slug: &str) -> CollectionDefinition {
        CollectionDefinition::new(slug)
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
        GlobalDefinition::new(slug)
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

    #[test]
    fn register_and_get_richtext_node() {
        let mut reg = Registry::new();
        assert!(reg.get_richtext_node("cta").is_none());

        reg.register_richtext_node(
            RichtextNodeDef::builder("cta", "Call to Action")
                .inline(false)
                .attrs(vec![
                    NodeAttr::builder("text", "Button Text")
                        .attr_type(NodeAttrType::Text)
                        .required(true)
                        .build(),
                ])
                .searchable_attrs(vec!["text".to_string()])
                .has_render(false)
                .build(),
        );
        let node = reg.get_richtext_node("cta").unwrap();
        assert_eq!(node.name, "cta");
        assert_eq!(node.label, "Call to Action");
        assert!(!node.inline);
        assert_eq!(node.attrs.len(), 1);
    }

    #[test]
    fn snapshot_clones_registry() {
        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(make_collection("posts"));
            reg.register_global(make_global("settings"));
        }
        let snap = Registry::snapshot(&shared);
        assert!(snap.get_collection("posts").is_some());
        assert!(snap.get_global("settings").is_some());
        assert_eq!(snap.collections.len(), 1);
        assert_eq!(snap.globals.len(), 1);
    }
}
