//! Builder for [`HookRunner`].

use std::{fs, path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use mlua::{Lua, LuaOptions, StdLib};
use tracing::{debug, info};

use crate::{
    config::CrapConfig,
    core::{Registry, SharedInvalidationTransport, upload},
    db::query::SharedPopulateSingleflight,
    hooks::{
        self, HookRunner,
        lifecycle::{
            InitPhase, LuaInvalidationTransport, LuaPopulateSingleflight, LuaStorage,
            execution::scan_registered_events,
            types::{DefaultDeny, HookDepth, LuaLocaleConfig, MaxHookDepth, MaxInstructions},
        },
        lua_api::{
            self, VmLabel,
            crud::register_crud_functions,
            register::{register_any_factories, register_per_slug_accessors},
        },
    },
};

use super::vm_pool::VmPool;

/// Builder for [`HookRunner`]. Created via [`HookRunner::builder`].
pub struct HookRunnerBuilder<'a> {
    config_dir: Option<&'a Path>,
    registry: Option<Arc<Registry>>,
    config: Option<&'a CrapConfig>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
}

impl<'a> HookRunnerBuilder<'a> {
    pub(super) fn new() -> Self {
        Self {
            config_dir: None,
            registry: None,
            config: None,
            invalidation_transport: None,
            populate_singleflight: None,
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

    /// Attach the process-wide populate singleflight to every VM in the pool
    /// so Lua-driven `crap.collections.find` / `find_by_id` calls can dedup
    /// populate cache-miss fetches across concurrent requests. For
    /// override-access Lua calls the service layer's guardrail discards this
    /// Arc, so the thread-through only pays off for ordinary (non-override)
    /// Lua reads.
    #[must_use]
    pub fn populate_singleflight(mut self, singleflight: SharedPopulateSingleflight) -> Self {
        self.populate_singleflight = Some(singleflight);
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
        let populate_singleflight = self.populate_singleflight;

        let pool_size = config.hooks.vm_pool_size.max(1);

        debug!("HookRunner: creating pool of {} Lua VMs", pool_size);

        let start = std::time::Instant::now();
        let mut vms = Vec::with_capacity(pool_size);

        for i in 0..pool_size {
            vms.push(create_lua_vm(
                config_dir,
                &registry,
                config,
                i + 1,
                invalidation_transport.clone(),
                populate_singleflight.clone(),
            )?);
        }

        let elapsed = start.elapsed();

        // Cache which events have globally-registered hooks (from init.lua).
        // All VMs execute the same init.lua, so checking any VM suffices.
        let registered_events = scan_registered_events(&vms[0]);

        info!(
            "HookRunner ready: {} VM(s) in {:.0}ms{}",
            pool_size,
            elapsed.as_secs_f64() * 1000.0,
            if registered_events.is_empty() {
                String::new()
            } else {
                format!(", global events: {registered_events:?}")
            }
        );

        Ok(HookRunner {
            pool: Arc::new(VmPool::new(vms)),
            registered_events: Arc::new(registered_events),
            registry,
        })
    }
}

/// Create and fully initialize a single Lua VM with package paths, API, CRUD functions,
/// collection/global/job loading, and init.lua execution.
fn create_lua_vm(
    config_dir: &Path,
    registry: &Arc<Registry>,
    config: &CrapConfig,
    vm_index: usize,
    invalidation_transport: Option<SharedInvalidationTransport>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
) -> Result<Lua> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;

    hooks::sandbox_lua(&lua)?;

    if config.hooks.max_memory > 0 {
        // 32-bit overflow path falls back to 256 MiB (a sane VM memory ceiling)
        // rather than usize::MAX, which would effectively disable the limit.
        let memory_limit = usize::try_from(config.hooks.max_memory).unwrap_or(256 * 1024 * 1024);
        lua.set_memory_limit(memory_limit)?;
    }

    lua.set_app_data(VmLabel(format!("vm-{vm_index}")));

    setup_package_paths(&lua, config_dir)?;

    register_apis(&lua, registry, config)?;

    init_app_data(
        &lua,
        config_dir,
        config,
        invalidation_transport,
        populate_singleflight,
    )?;

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
    load_def_dir(&lua, config_dir, "job")?;

    execute_init_lua(&lua, config_dir, vm_index)?;

    lua.remove_app_data::<InitPhase>();

    Ok(lua)
}

/// Set up Lua package.path to include the config directory.
fn setup_package_paths(lua: &Lua, config_dir: &Path) -> Result<()> {
    let config_str = config_dir.to_string_lossy();
    let code =
        format!(r#"package.path = "{config_str}/?.lua;{config_str}/?/init.lua;" .. package.path"#);

    lua.load(&code)
        .exec()
        .context("Failed to set package paths")
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
        &config.locale,
        &config.pagination,
        &config.jobs,
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

/// Initialize hook depth tracking, access config, and storage backend.
fn init_app_data(
    lua: &Lua,
    config_dir: &Path,
    config: &CrapConfig,
    invalidation_transport: Option<SharedInvalidationTransport>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
) -> Result<()> {
    lua.set_app_data(HookDepth(0));
    lua.set_app_data(MaxHookDepth(config.hooks.max_depth));
    lua.set_app_data(DefaultDeny(config.access.default_deny));
    lua.set_app_data(MaxInstructions(config.hooks.max_instructions));
    lua.set_app_data(LuaLocaleConfig(config.locale.clone()));

    let storage = upload::create_storage(config_dir, &config.upload)
        .context("Failed to create storage backend for Lua VM")?;

    lua.set_app_data(LuaStorage(storage));

    if let Some(transport) = invalidation_transport {
        lua.set_app_data(LuaInvalidationTransport(transport));
    }

    if let Some(sf) = populate_singleflight {
        lua.set_app_data(LuaPopulateSingleflight(sf));
    }

    Ok(())
}

/// Execute init.lua if it exists in the config directory.
fn execute_init_lua(lua: &Lua, config_dir: &Path, vm_index: usize) -> Result<()> {
    let init_path = config_dir.join("init.lua");

    if init_path.exists() {
        debug!("[lua:vm-{vm_index}] Executing init.lua");

        let code = fs::read_to_string(&init_path)
            .with_context(|| format!("Failed to read {}", init_path.display()))?;

        lua.load(&code)
            .set_name(init_path.to_string_lossy())
            .exec()
            .with_context(|| "HookRunner: failed to execute init.lua")?;
    }

    Ok(())
}

/// Load Lua definition files from `{config_dir}/{kind}s/` if the directory exists.
/// Passes the plural directory name as the `package.loaded` cache-key
/// prefix so `require("jobs.foo")` hits the cache populated for
/// `<config_dir>/jobs/foo.lua`.
fn load_def_dir(lua: &Lua, config_dir: &Path, kind: &str) -> Result<()> {
    let dir_name = format!("{kind}s");
    let dir = config_dir.join(&dir_name);

    if dir.exists() {
        hooks::load_lua_dir(lua, &dir, &dir_name)?;
    }

    Ok(())
}
