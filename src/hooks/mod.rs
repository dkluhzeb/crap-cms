//! Lua VM setup, `crap.*` API registration, and hook lifecycle management.

pub mod api;
pub mod lifecycle;

pub use lifecycle::{HookContext, HookEvent, HookRunner, ValidationCtx};

use anyhow::{Context as _, Result};
use mlua::{Lua, StdLib};
use std::path::Path;

use crate::{config::CrapConfig, core::SharedRegistry};

/// Initialize the Lua VM, register the crap API, load collections/globals,
/// and run init.lua. Returns a populated SharedRegistry.
pub fn init_lua(config_dir: &Path, config: &CrapConfig) -> Result<SharedRegistry> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, mlua::LuaOptions::default())?;
    sandbox_lua(&lua)?;
    lua.set_app_data(api::VmLabel("init".to_string()));
    let registry = crate::core::Registry::shared();

    // Set up package paths rooted at config dir
    setup_package_paths(&lua, config_dir)?;

    // Register the crap global API table
    api::register_api(&lua, registry.clone(), config_dir, config)?;

    // Auto-load collections/*.lua
    let collections_dir = config_dir.join("collections");
    let n_collections = if collections_dir.exists() {
        load_lua_dir(&lua, &collections_dir, "collection")?
    } else {
        0
    };

    // Auto-load globals/*.lua
    let globals_dir = config_dir.join("globals");
    let n_globals = if globals_dir.exists() {
        load_lua_dir(&lua, &globals_dir, "global")?
    } else {
        0
    };

    // Auto-load jobs/*.lua
    let jobs_dir = config_dir.join("jobs");
    let n_jobs = if jobs_dir.exists() {
        load_lua_dir(&lua, &jobs_dir, "job")?
    } else {
        0
    };

    // Execute init.lua if present
    let init_path = config_dir.join("init.lua");
    let has_init = init_path.exists();

    if has_init {
        tracing::debug!("[lua:init] Executing init.lua");
        let code = std::fs::read_to_string(&init_path)
            .with_context(|| format!("Failed to read {}", init_path.display()))?;
        lua.load(&code)
            .set_name(init_path.to_string_lossy())
            .exec()
            .with_context(|| "Failed to execute init.lua")?;
    }

    tracing::info!(
        "Lua init: loaded {} collection(s), {} global(s), {} job(s){}",
        n_collections,
        n_globals,
        n_jobs,
        if has_init { ", executed init.lua" } else { "" }
    );

    Ok(registry)
}

fn setup_package_paths(lua: &Lua, config_dir: &Path) -> Result<()> {
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
    Ok(())
}

/// Apply sandbox restrictions to a Lua VM.
/// Re-adds the `os` library with only safe functions (time, date, clock, difftime)
/// and removes dangerous globals (loadfile, dofile).
pub(crate) fn sandbox_lua(lua: &Lua) -> Result<()> {
    // ALL_SAFE excludes `os`, `io`, `ffi`, and `debug`.
    // Re-add os with only safe functions.
    lua.load_std_libs(StdLib::OS)?;
    let os: mlua::Table = lua.globals().get("os")?;
    os.set("execute", mlua::Value::Nil)?;
    os.set("remove", mlua::Value::Nil)?;
    os.set("rename", mlua::Value::Nil)?;
    os.set("exit", mlua::Value::Nil)?;
    os.set("tmpname", mlua::Value::Nil)?;
    os.set("getenv", mlua::Value::Nil)?;
    os.set("setlocale", mlua::Value::Nil)?;
    // Keeps: os.time, os.date, os.clock, os.difftime

    // Remove remaining dangerous globals
    lua.globals().set("loadfile", mlua::Value::Nil)?;
    lua.globals().set("dofile", mlua::Value::Nil)?;

    Ok(())
}

/// Load and execute all `.lua` files in a directory (used for `collections/` and `globals/`).
/// Returns the number of files loaded.
pub(crate) fn load_lua_dir(lua: &Lua, dir: &Path, kind: &str) -> Result<usize> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read {} directory: {}", kind, dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "lua"))
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let count = entries.len();
    for entry in entries {
        let path = entry.path();
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy(),
            None => continue,
        };
        let label = lua
            .app_data_ref::<api::VmLabel>()
            .map(|l| l.0.clone())
            .unwrap_or_else(|| "lua".into());
        tracing::debug!("[lua:{label}] Loading {kind}: {name}");

        let code = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        lua.load(&code)
            .set_name(path.to_string_lossy())
            .exec()
            .with_context(|| format!("Failed to execute {}", path.display()))?;
    }

    Ok(count)
}
