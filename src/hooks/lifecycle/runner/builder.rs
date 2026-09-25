//! Builder for [`HookRunner`].

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context as _, Result};
use mlua::{Lua, LuaOptions, StdLib};
use tracing::{debug, info};

use crate::{
    config::CacheBackend as CacheBackendCfg,
    config::{CrapConfig, UploadStorage},
    core::{
        LocalLease, Registry, SharedInvalidationTransport,
        cache::{CustomCache, SharedCache},
        upload,
    },
    hooks::{
        self, HookRunner, IoJail,
        lifecycle::{
            InitPhase, LuaVmInfra,
            execution::{scan_has_template_data, scan_registered_events},
            types::HookDepth,
        },
        lua_api::{
            self, VmLabel,
            crud::{CrudConfig, register_crud_functions},
            register::{register_any_factories, register_per_slug_accessors},
        },
    },
};

use super::vm_pool::{VmFactory, VmPool, apply_vm_limits};

/// Builder for [`HookRunner`]. Created via [`HookRunner::builder`].
pub struct HookRunnerBuilder<'a> {
    config_dir: Option<&'a Path>,
    registry: Option<Arc<Registry>>,
    config: Option<&'a CrapConfig>,
    invalidation_transport: Option<SharedInvalidationTransport>,
}

impl<'a> HookRunnerBuilder<'a> {
    pub(super) fn new() -> Self {
        Self {
            config_dir: None,
            registry: None,
            config: None,
            invalidation_transport: None,
        }
    }

    #[must_use]
    pub fn config_dir(mut self, config_dir: &'a Path) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    #[must_use]
    pub fn registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = Some(registry);
        self
    }

    #[must_use]
    pub fn config(mut self, config: &'a CrapConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Attach the user-invalidation transport to every VM in the pool so
    /// Lua-driven delete / lock paths can tear down live-update streams.
    #[must_use]
    pub fn invalidation_transport(mut self, transport: SharedInvalidationTransport) -> Self {
        self.invalidation_transport = Some(transport);
        self
    }

    /// Build the `HookRunner`, creating and initializing the Lua VM pool.
    ///
    /// # Errors
    ///
    /// Returns an error if any Lua VM in the pool fails to initialize.
    ///
    /// # Panics
    ///
    /// Panics if `config_dir`, `registry`, or `config` was not set on the builder.
    pub fn build(self) -> Result<HookRunner> {
        let config_dir = self.config_dir.expect("config_dir is required");
        let registry = self.registry.expect("registry is required");
        let config = self.config.expect("config is required");
        let invalidation_transport = self.invalidation_transport;

        let pool_size = config.hooks.vm_pool_size.max(1);
        let cap = config.hooks.max_vm_pool_size.max(pool_size);

        debug!(
            "HookRunner: pre-warming {} Lua VM(s) (cap {})",
            pool_size, cap
        );

        // Factory for on-demand growth: owns everything `create_lua_vm` needs
        // so a fresh VM can be built on any blocking thread when concurrency
        // exceeds the pre-warmed set. The io jail is resolved once here and
        // shared by every VM.
        let blueprint = VmBlueprint {
            config_dir: config_dir.to_path_buf(),
            registry: Arc::clone(&registry),
            config: config.clone(),
            invalidation_transport,
            io_jail: Arc::new(IoJail::new(config_dir, config)?),
        };
        let factory: VmFactory = Box::new(move |idx| create_lua_vm(&blueprint, idx));

        let start = Instant::now();
        let mut prewarmed = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            prewarmed.push(factory(i + 1)?);
        }
        let elapsed = start.elapsed();

        // Cache which events have globally-registered hooks (from init.lua).
        // All VMs execute the same init.lua, so checking any VM suffices.
        let registered_events = scan_registered_events(&prewarmed[0]);
        let has_template_data = scan_has_template_data(&prewarmed[0]);

        info!(
            "HookRunner ready: {} VM(s) pre-warmed in {:.0}ms (cap {}){}",
            pool_size,
            elapsed.as_secs_f64() * 1000.0,
            cap,
            if registered_events.is_empty() {
                String::new()
            } else {
                format!(", global events: {registered_events:?}")
            }
        );

        Ok(HookRunner {
            pool: Arc::new(VmPool::new(prewarmed, factory, cap)),
            registered_events: Arc::new(registered_events),
            has_template_data,
            registry,
            default_deny: config.access.default_deny,
        })
    }
}

/// Everything a pool VM is built from — owned by the pool's factory so a VM
/// can be built on demand on any blocking thread.
struct VmBlueprint {
    config_dir: PathBuf,
    registry: Arc<Registry>,
    config: CrapConfig,
    invalidation_transport: Option<SharedInvalidationTransport>,
    io_jail: Arc<IoJail>,
}

/// Create and fully initialize a single Lua VM with package paths, API, CRUD functions,
/// collection/global/job loading, and init.lua execution.
fn create_lua_vm(blueprint: &VmBlueprint, vm_index: usize) -> Result<Lua> {
    let VmBlueprint {
        config_dir,
        registry,
        config,
        io_jail,
        ..
    } = blueprint;

    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;

    hooks::sandbox_lua(&lua, io_jail)?;

    apply_vm_limits(&lua, &config.hooks)?;

    lua.set_app_data(VmLabel(format!("vm-{vm_index}")));

    hooks::install_module_loader(&lua, config_dir, io_jail)?;

    register_apis(&lua, registry, config)?;

    init_app_data(&lua, blueprint)?;

    // Mark the init phase so register-only APIs (`crap.pages.register`,
    // `crap.template_data.register`, ...) accept calls. The marker is
    // removed at the end so any later runtime hook that calls those APIs
    // gets a clear error instead of a silent no-op or per-VM fragmentation.
    lua.set_app_data(InitPhase);

    // Pool VMs skip `collections/` and `globals/` files — those files
    // only call `crap.<x>.define`, which writes to the shared registry.
    // The init_lua VM already populated the registry; pool VMs hold
    // the resulting `Arc<Registry>` snapshot. Re-running these files
    // here would only double-write idempotently.
    //
    // `jobs/` IS re-run because its files contain handler functions
    // (`local M = {} ; function M.run(ctx) ... end ; return M`) that
    // produce per-VM Lua state — the `M.run` handle is bound to this
    // VM and the dispatcher's later `require("jobs.foo")` must hit
    // this VM's `package.loaded` cache to find it.
    hooks::load_def_dir(&lua, config_dir, "job")?;

    hooks::execute_init_lua(&lua, config_dir).context("HookRunner: failed to execute init.lua")?;

    lua.remove_app_data::<InitPhase>();

    Ok(lua)
}

/// Register the crap API and CRUD functions on pool Lua VMs. Pool VMs
/// hold the `Arc<Registry>` snapshot for both the API surface and the
/// CRUD layer — the registry is fully populated by the time `HookRunner`
/// is built, and `crap.<x>.define` calls during pool VM init are
/// no-ops (the `init_lua` VM already wrote those defs).
fn register_apis(lua: &Lua, registry: &Arc<Registry>, config: &CrapConfig) -> Result<()> {
    lua_api::register_api_pool_init(lua, Arc::clone(registry), config)?;
    register_crud_functions(
        lua,
        Arc::clone(registry),
        &CrudConfig {
            locale: &config.locale,
            pagination: &config.pagination,
            depth: &config.depth,
            jobs: &config.jobs,
            bulk_max_documents: config.server.bulk_max_documents,
            password_policy: &config.auth.password_policy,
        },
    )?;
    // Per-collection / per-global accessors at `crap.collections.<slug>`
    // / `crap.globals.<slug>` — typed wrappers that bind the slug and
    // dispatch to the slug-keyed CRUD API. Runs LAST so every method
    // they wrap exists on `crap.collections` / `crap.globals`.
    register_per_slug_accessors(lua, registry)?;
    // `crap.any.*` — pass-through typing helpers for cross-collection
    // callables. Pure no-ops at runtime; their value is letting LuaLS
    // propagate callback param types via `Lua.type.inferParamType`.
    register_any_factories(lua)?;

    Ok(())
}

/// Initialize hook depth tracking and the VM-stable infrastructure bundle ([`LuaVmInfra`]).
fn init_app_data(lua: &Lua, blueprint: &VmBlueprint) -> Result<()> {
    let VmBlueprint {
        config_dir,
        registry,
        config,
        invalidation_transport,
        ..
    } = blueprint;

    lua.set_app_data(HookDepth(0));

    // Inside a pool VM, a custom backend delegates to `crap._storage`
    // (set by `crap.storage.register` during this VM's init.lua). Back it
    // with a `LocalLease` over the current VM so CRUD-delete reuses that
    // VM rather than re-acquiring from the pool (which would deadlock).
    let storage: upload::SharedStorage = if matches!(config.upload.storage, UploadStorage::Custom) {
        Arc::new(upload::storage::CustomStorage::new(Arc::new(
            LocalLease::new(lua),
        )))
    } else {
        upload::create_storage(config_dir, &config.upload)
            .context("Failed to create storage backend for Lua VM")?
    };

    // Same per-VM treatment for a custom cache: write-through `clear_cache`
    // from inside this VM must reuse THIS VM via a `LocalLease`, never
    // re-acquire from the pool.
    let cache: Option<SharedCache> = matches!(config.cache.backend, CacheBackendCfg::Custom)
        .then(|| Arc::new(CustomCache::new(Arc::new(LocalLease::new(lua)))) as SharedCache);

    lua.set_app_data(LuaVmInfra {
        registry: Arc::clone(registry),
        locale_config: config.locale.clone(),
        storage: Some(storage),
        cache,
        invalidation_transport: invalidation_transport.clone(),
        max_hook_depth: config.hooks.max_depth,
        default_deny: config.access.default_deny,
    });

    Ok(())
}
