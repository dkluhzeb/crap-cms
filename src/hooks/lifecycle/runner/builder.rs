//! Builder for [`HookRunner`].

use std::{fs, path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use mlua::{Lua, LuaOptions, StdLib};
use tracing::{debug, info};

use crate::{
    config::CrapConfig,
    core::{Registry, SharedRegistry, event::SharedInvalidationTransport, upload},
    db::query::SharedPopulateSingleflight,
    hooks::{
        self, HookRunner,
        api::{self, VmLabel},
        lifecycle::{
            LuaInvalidationTransport, LuaPopulateSingleflight, LuaStorage,
            crud::register_crud_functions,
            execution::scan_registered_events,
            types::{DefaultDeny, HookDepth, MaxHookDepth, MaxInstructions},
        },
    },
};

use super::vm_pool::VmPool;

/// Builder for [`HookRunner`]. Created via [`HookRunner::builder`].
pub struct HookRunnerBuilder<'a> {
    config_dir: Option<&'a Path>,
    registry: Option<SharedRegistry>,
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

    pub fn config_dir(mut self, config_dir: &'a Path) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    pub fn registry(mut self, registry: SharedRegistry) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn config(mut self, config: &'a CrapConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Attach the user-invalidation transport to every VM in the pool so
    /// Lua-driven delete / lock paths can tear down live-update streams.
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
    pub fn populate_singleflight(mut self, singleflight: SharedPopulateSingleflight) -> Self {
        self.populate_singleflight = Some(singleflight);
        self
    }

    /// Build the HookRunner, creating and initializing the Lua VM pool.
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
                registry.clone(),
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
                format!(", global events: {:?}", registered_events)
            }
        );

        let registry_snapshot = Registry::snapshot(&registry);

        Ok(HookRunner {
            pool: Arc::new(VmPool::new(vms)),
            registered_events: Arc::new(registered_events),
            registry: registry_snapshot,
        })
    }
}

/// Create and fully initialize a single Lua VM with package paths, API, CRUD functions,
/// collection/global/job loading, and init.lua execution.
fn create_lua_vm(
    config_dir: &Path,
    registry: SharedRegistry,
    config: &CrapConfig,
    vm_index: usize,
    invalidation_transport: Option<SharedInvalidationTransport>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
) -> Result<Lua> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;

    hooks::sandbox_lua(&lua)?;

    if config.hooks.max_memory > 0 {
        lua.set_memory_limit(config.hooks.max_memory as usize)?;
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

    load_def_dir(&lua, config_dir, "collection")?;
    load_def_dir(&lua, config_dir, "global")?;
    load_def_dir(&lua, config_dir, "job")?;

    execute_init_lua(&lua, config_dir, vm_index)?;

    Ok(lua)
}

/// Set up Lua package.path to include the config directory.
fn setup_package_paths(lua: &Lua, config_dir: &Path) -> Result<()> {
    let config_str = config_dir.to_string_lossy();
    let code = format!(
        r#"package.path = "{0}/?.lua;{0}/?/init.lua;" .. package.path"#,
        config_str
    );

    lua.load(&code)
        .exec()
        .context("Failed to set package paths")
}

/// Register the crap API and CRUD functions on the Lua VM.
fn register_apis(lua: &Lua, registry: SharedRegistry, config: &CrapConfig) -> Result<()> {
    api::register_api(lua, registry.clone(), config)?;

    register_crud_functions(lua, registry, &config.locale, &config.pagination)?;

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
fn load_def_dir(lua: &Lua, config_dir: &Path, kind: &str) -> Result<()> {
    let dir = config_dir.join(format!("{kind}s"));

    if dir.exists() {
        hooks::load_lua_dir(lua, &dir, kind)?;
    }

    Ok(())
}
