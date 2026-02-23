use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use super::collection::{CollectionDefinition, GlobalDefinition};

pub struct Registry {
    pub collections: HashMap<String, CollectionDefinition>,
    pub globals: HashMap<String, GlobalDefinition>,
}

pub type SharedRegistry = Arc<RwLock<Registry>>;

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            collections: HashMap::new(),
            globals: HashMap::new(),
        }
    }

    pub fn shared() -> SharedRegistry {
        Arc::new(RwLock::new(Self::new()))
    }

    pub fn register_collection(&mut self, def: CollectionDefinition) {
        tracing::debug!("Registering collection '{}'", def.slug);
        self.collections.insert(def.slug.clone(), def);
    }

    pub fn register_global(&mut self, def: GlobalDefinition) {
        tracing::debug!("Registering global '{}'", def.slug);
        self.globals.insert(def.slug.clone(), def);
    }

    pub fn get_collection(&self, slug: &str) -> Option<&CollectionDefinition> {
        self.collections.get(slug)
    }

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
}
