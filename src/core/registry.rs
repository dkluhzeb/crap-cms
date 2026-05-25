//! In-memory registry of collection and global definitions loaded from Lua.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
use tracing::{debug, warn};

use crate::core::{
    CollectionDefinition, FieldDefinition, FieldType, Slug,
    collection::{Activation, AuthMethod, GlobalDefinition, Surface},
    job::JobDefinition,
    richtext::RichtextNodeDef,
};

/// Materialised pointer to a single `AuthMethod::Strategy` in the
/// registry. Carries the bits the evaluator needs to invoke the hook
/// without re-walking the collection tree per request.
#[derive(Debug, Clone)]
pub struct StrategyEntry {
    /// Collection slug the strategy was declared on. Threaded into
    /// the Lua hook as `ctx.collection` and used to look up the
    /// collection's auth config (locked / `verify_email` / token
    /// expiry) when a strategy hit produces a user document.
    pub slug: Slug,
    /// Identifier used in logs and `ResolvedMethod::Strategy.name`.
    pub name: String,
    /// Lua function reference (`module.function`) for the strategy.
    pub authenticate: String,
}

/// Holds all collection, global, and job definitions loaded at startup.
///
/// `always_strategies` and `header_strategies` are precomputed indexes
/// derived from the strategy `AuthMethod`s in `collections`. They're
/// built by [`Self::rebuild_strategy_index`] (called at snapshot
/// time) so the per-request auth evaluator can answer "does any
/// strategy fire for this request?" without walking every collection
/// × method on every call. Previously every gRPC request paid an
/// O(collections × methods × activation-checks) scan even when
/// nothing was going to match.
#[derive(Clone, Default)]
pub struct Registry {
    pub collections: HashMap<Slug, CollectionDefinition>,
    pub globals: HashMap<Slug, GlobalDefinition>,
    pub jobs: HashMap<Slug, JobDefinition>,
    pub richtext_nodes: HashMap<String, RichtextNodeDef>,
    /// `Activation::Always`-activated strategies grouped by the
    /// surface they fire on. Looked up once per request to enumerate
    /// strategies that run unconditionally for that surface.
    pub always_strategies: HashMap<Surface, Vec<StrategyEntry>>,
    /// `Activation::Header { header }` strategies grouped by
    /// `(lowercased-header-name, surface)`. Looked up once per
    /// request-header to enumerate strategies discriminated by that
    /// header on that surface. Header names are stored lowercased so
    /// the lookup matches HTTP / gRPC metadata case-insensitivity
    /// without per-request normalisation.
    pub header_strategies: HashMap<(String, Surface), Vec<StrategyEntry>>,
}

/// Thread-safe shared reference to the registry. Used during init when
/// definitions are being mutated; runtime VMs hold an `Arc<Registry>`
/// snapshot instead.
pub type SharedRegistry = Arc<RwLock<Registry>>;

/// Read-only registry handle. Implemented for both [`SharedRegistry`]
/// (init-phase, locks per call) and `Arc<Registry>` (runtime snapshot,
/// no lock), so registration helpers can stay flavour-agnostic and a
/// single function body covers both phases. Each call to [`with`]
/// borrows the underlying [`Registry`] for the duration of the closure.
pub trait RegistryRead: Clone + Send + Sync + 'static {
    /// Invoke `f` with a borrow of the underlying [`Registry`]. Returns
    /// `Err` if the underlying lock is poisoned (only possible for the
    /// `SharedRegistry` implementation).
    ///
    /// # Errors
    ///
    /// Returns [`RegistryLockPoisoned`] when the `SharedRegistry` mutex is
    /// poisoned. The `Arc<Registry>` snapshot implementation never errors.
    fn with<R>(&self, f: impl FnOnce(&Registry) -> R) -> Result<R, RegistryLockPoisoned>;
}

/// Returned by [`RegistryRead::with`] when the underlying lock is
/// poisoned. Only the `SharedRegistry` implementation can produce this.
#[derive(Debug, Clone, Copy)]
pub struct RegistryLockPoisoned;

impl std::fmt::Display for RegistryLockPoisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Registry lock poisoned")
    }
}

impl std::error::Error for RegistryLockPoisoned {}

impl RegistryRead for SharedRegistry {
    fn with<R>(&self, f: impl FnOnce(&Registry) -> R) -> Result<R, RegistryLockPoisoned> {
        let r = self.read().map_err(|_| RegistryLockPoisoned)?;
        Ok(f(&r))
    }
}

impl RegistryRead for Arc<Registry> {
    fn with<R>(&self, f: impl FnOnce(&Registry) -> R) -> Result<R, RegistryLockPoisoned> {
        Ok(f(self))
    }
}

impl Registry {
    /// Create an empty registry with no collections or globals.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a new registry wrapped in `Arc<RwLock<>>` for shared ownership.
    #[must_use]
    pub fn shared() -> SharedRegistry {
        Arc::new(RwLock::new(Self::default()))
    }

    /// Register a collection definition, keyed by slug. Overwrites any existing definition.
    pub fn register_collection(&mut self, def: CollectionDefinition) {
        debug!("Registering collection '{}'", def.slug);

        Self::warn_invalid_field_refs(&def);

        self.collections.insert(def.slug.clone(), def);
    }

    /// Log warnings for admin config fields that reference nonexistent field names.
    fn warn_invalid_field_refs(def: &CollectionDefinition) {
        let slug = &def.slug;

        if let Some(title) = def.title_field()
            && !Self::field_exists_recursive(title, &def.fields)
        {
            warn!(
                "Collection '{}': use_as_title references '{}' which is not a field",
                slug, title
            );
        }

        if let Some(ref sort) = def.admin.default_sort {
            let col = sort.strip_prefix('-').unwrap_or(sort);
            if !matches!(
                col,
                "id" | "created_at" | "updated_at" | "_status" | "_deleted_at"
            ) && !Self::field_exists_recursive(col, &def.fields)
            {
                warn!(
                    "Collection '{}': default_sort references '{}' which is not a field",
                    slug, col
                );
            }
        }

        for name in &def.admin.list_searchable_fields {
            if !Self::field_exists_recursive(name, &def.fields) {
                warn!(
                    "Collection '{}': list_searchable_fields references '{}' which is not a field",
                    slug, name
                );
            }
        }
    }

    /// Check if a field name exists, recursing into layout wrappers.
    fn field_exists_recursive(name: &str, fields: &[FieldDefinition]) -> bool {
        fields.iter().any(|f| {
            if f.name == name {
                return true;
            }

            match f.field_type {
                FieldType::Row | FieldType::Collapsible => {
                    Self::field_exists_recursive(name, &f.fields)
                }
                FieldType::Tabs => f
                    .tabs
                    .iter()
                    .any(|tab| Self::field_exists_recursive(name, &tab.fields)),
                _ => false,
            }
        })
    }

    /// Register a global definition, keyed by slug. Overwrites any existing definition.
    pub fn register_global(&mut self, def: GlobalDefinition) {
        debug!("Registering global '{}'", def.slug);

        self.globals.insert(def.slug.clone(), def);
    }

    /// Look up a collection definition by slug.
    #[must_use]
    pub fn get_collection(&self, slug: &str) -> Option<&CollectionDefinition> {
        self.collections.get(slug)
    }

    /// Look up a global definition by slug.
    #[must_use]
    pub fn get_global(&self, slug: &str) -> Option<&GlobalDefinition> {
        self.globals.get(slug)
    }

    /// Register a job definition, keyed by slug. Overwrites any existing definition.
    pub fn register_job(&mut self, def: JobDefinition) {
        debug!("Registering job '{}'", def.slug);

        self.jobs.insert(def.slug.clone(), def);
    }

    /// Look up a job definition by slug.
    #[must_use]
    pub fn get_job(&self, slug: &str) -> Option<&JobDefinition> {
        self.jobs.get(slug)
    }

    /// Register a custom richtext node definition, keyed by name.
    pub fn register_richtext_node(&mut self, def: RichtextNodeDef) {
        debug!("Registering richtext node '{}'", def.name);

        self.richtext_nodes.insert(def.name.clone(), def);
    }

    /// Look up a custom richtext node definition by name.
    #[must_use]
    pub fn get_richtext_node(&self, name: &str) -> Option<&RichtextNodeDef> {
        self.richtext_nodes.get(name)
    }

    /// Create a read-only `Arc<Registry>` snapshot from a `SharedRegistry`.
    ///
    /// Call once after startup (after all `define()` writes) and pass the snapshot
    /// to hot-path consumers (admin UI, gRPC API) that only read the registry.
    ///
    /// # Panics
    ///
    /// Panics if the registry's `RwLock` is poisoned (another thread panicked
    /// while holding the write lock during startup).
    pub fn snapshot(shared: &SharedRegistry) -> Arc<Registry> {
        let reg = shared
            .read()
            .expect("Registry lock poisoned during snapshot");
        let mut snap = reg.clone();
        snap.rebuild_strategy_index();
        Arc::new(snap)
    }

    /// Recompute [`Self::always_strategies`] and
    /// [`Self::header_strategies`] from the current `collections`
    /// state. Idempotent — clears and refills both maps. Called once
    /// at [`Self::snapshot`] time so runtime callers reuse the same
    /// indexes for every request. Cost is O(collections × methods),
    /// paid once at startup instead of per request.
    pub fn rebuild_strategy_index(&mut self) {
        self.always_strategies.clear();
        self.header_strategies.clear();

        for (slug, def) in &self.collections {
            let Some(auth) = def.auth.as_ref() else {
                continue;
            };
            if !auth.enabled {
                continue;
            }
            for method in &auth.methods {
                let AuthMethod::Strategy {
                    name,
                    authenticate,
                    activates_on,
                    surfaces,
                } = method
                else {
                    continue;
                };
                let entry = StrategyEntry {
                    slug: slug.clone(),
                    name: name.clone(),
                    authenticate: authenticate.clone(),
                };
                for surface in surfaces {
                    match activates_on {
                        Activation::Always { .. } => {
                            self.always_strategies
                                .entry(*surface)
                                .or_default()
                                .push(entry.clone());
                        }
                        Activation::Header { header } => {
                            self.header_strategies
                                .entry((header.to_ascii_lowercase(), *surface))
                                .or_default()
                                .push(entry.clone());
                        }
                    }
                }
            }
        }
    }

    /// Whether any collection's `auth.methods` contains a
    /// `Strategy` variant. Used by the auth evaluator and the gRPC
    /// metadata-header extractor to short-circuit per-request work
    /// when no strategy is configured: an anonymous request on a
    /// deployment that only uses `password_login` / `bearer` /
    /// `session_cookie` does not need to walk every collection
    /// looking for a header-activated strategy, and does not need
    /// to materialise the request's metadata into a `HashMap` since
    /// no consumer will read it.
    ///
    /// Short-circuiting iterator — returns on the first strategy
    /// found. With a handful of collections + a handful of methods
    /// each, this is a few cmp-and-branches per call; cheaper than
    /// the work it gates by orders of magnitude.
    #[must_use]
    pub fn has_any_strategy(&self) -> bool {
        self.collections.values().any(|def| {
            def.auth.as_ref().is_some_and(|a| {
                a.enabled
                    && a.methods
                        .iter()
                        .any(|m| matches!(m, AuthMethod::Strategy { .. }))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldDefinition, FieldTab, FieldType, richtext::RichtextNodeDef};

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
                    FieldDefinition::builder("text", FieldType::Text)
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

    /// Wrap a child field inside a layout container for testing `field_exists_recursive`.
    fn wrap_in_container(container_type: FieldType, child_name: &str) -> Vec<FieldDefinition> {
        let child = FieldDefinition::builder(child_name, FieldType::Text).build();

        match container_type {
            FieldType::Tabs => vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new("Main", vec![child])])
                    .build(),
            ],
            _ => vec![
                FieldDefinition::builder("wrapper", container_type)
                    .fields(vec![child])
                    .build(),
            ],
        }
    }

    #[test]
    fn field_exists_recursive_finds_nested_fields() {
        for container in [FieldType::Row, FieldType::Collapsible, FieldType::Tabs] {
            let fields = wrap_in_container(container.clone(), "target");

            assert!(
                Registry::field_exists_recursive("target", &fields),
                "Should find field inside {container:?}"
            );

            assert!(
                !Registry::field_exists_recursive("nonexistent", &fields),
                "Should not find nonexistent field in {container:?}"
            );
        }
    }
}
