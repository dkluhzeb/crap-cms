//! Builder for [`HookRunner`].

use std::{fs, path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use mlua::{Lua, StdLib};

use crate::{
    config::CrapConfig,
    core::SharedRegistry,
    hooks::{
        self, HookRunner,
        api::{self, VmLabel},
        lifecycle::{
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
}

impl<'a> HookRunnerBuilder<'a> {
    pub(super) fn new() -> Self {
        Self {
            config_dir: None,
            registry: None,
            config: None,
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

    /// Build the HookRunner, creating and initializing the Lua VM pool.
    pub fn build(self) -> Result<HookRunner> {
        let config_dir = self.config_dir.expect("config_dir is required");
        let registry = self.registry.expect("registry is required");
        let config = self.config.expect("config is required");

        let pool_size = config.hooks.vm_pool_size.max(1);
        tracing::info!("HookRunner: creating pool of {} Lua VMs", pool_size);

        let mut vms = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            vms.push(create_lua_vm(config_dir, registry.clone(), config, i + 1)?);
        }

        // Cache which events have globally-registered hooks (from init.lua).
        // All VMs execute the same init.lua, so checking any VM suffices.
        let registered_events = scan_registered_events(&vms[0]);

        if !registered_events.is_empty() {
            tracing::info!("HookRunner: registered events: {:?}", registered_events);
        }

        Ok(HookRunner {
            pool: Arc::new(VmPool::new(vms)),
            registered_events: Arc::new(registered_events),
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
) -> Result<Lua> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, mlua::LuaOptions::default())?;
    hooks::sandbox_lua(&lua)?;
    if config.hooks.max_memory > 0 {
        lua.set_memory_limit(config.hooks.max_memory as usize)?;
    }
    lua.set_app_data(VmLabel(format!("vm-{}", vm_index)));

    // Set up package paths
    let config_str = config_dir.to_string_lossy();
    let code = format!(
        r#"
        package.path = "{0}/?.lua;{0}/?/init.lua;" .. package.path
        "#,
        config_str
    );
    lua.load(&code)
        .exec()
        .context("Failed to set package paths")?;

    // Register crap.log, crap.util, crap.collections.define, etc.
    api::register_api(&lua, registry.clone(), config_dir, config)?;

    // Register CRUD functions on crap.collections (find, find_by_id, create, update, delete).
    // These read the active transaction from Lua app_data when called inside hooks.
    register_crud_functions(&lua, registry, &config.locale, &config.pagination)?;

    // Initialize hook depth tracking
    lua.set_app_data(HookDepth(0));
    lua.set_app_data(MaxHookDepth(config.hooks.max_depth));
    lua.set_app_data(DefaultDeny(config.access.default_deny));
    lua.set_app_data(MaxInstructions(config.hooks.max_instructions));

    // Auto-load collections/*.lua, globals/*.lua, and jobs/*.lua
    let collections_dir = config_dir.join("collections");

    if collections_dir.exists() {
        let _ = hooks::load_lua_dir(&lua, &collections_dir, "collection")?;
    }
    let globals_dir = config_dir.join("globals");

    if globals_dir.exists() {
        let _ = hooks::load_lua_dir(&lua, &globals_dir, "global")?;
    }
    let jobs_dir = config_dir.join("jobs");

    if jobs_dir.exists() {
        let _ = hooks::load_lua_dir(&lua, &jobs_dir, "job")?;
    }

    // Execute init.lua so crap.hooks.register() calls take effect in this VM
    let init_path = config_dir.join("init.lua");

    if init_path.exists() {
        tracing::debug!("[lua:vm-{vm_index}] Executing init.lua");
        let code = fs::read_to_string(&init_path)
            .with_context(|| format!("Failed to read {}", init_path.display()))?;
        lua.load(&code)
            .set_name(init_path.to_string_lossy())
            .exec()
            .with_context(|| "HookRunner: failed to execute init.lua")?;
    }

    Ok(lua)
}
