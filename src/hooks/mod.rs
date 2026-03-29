//! Lua VM setup, `crap.*` API registration, and hook lifecycle management.

pub mod api;
pub mod lifecycle;

pub use lifecycle::{HookContext, HookEvent, HookRunner, ValidationCtx};

use anyhow::{Context as _, Result};
use mlua::{Lua, LuaOptions, StdLib, Table};
use std::{fs, path::Path};

use crate::{
    config::CrapConfig,
    core::{FieldDefinition, FieldType, Registry, SharedRegistry},
};

/// Initialize the Lua VM, register the crap API, load collections/globals,
/// and run init.lua. Returns a populated SharedRegistry.
pub fn init_lua(config_dir: &Path, config: &CrapConfig) -> Result<SharedRegistry> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
    sandbox_lua(&lua)?;
    lua.set_app_data(api::VmLabel("init".to_string()));
    let registry = Registry::shared();

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
        let code = fs::read_to_string(&init_path)
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

    // Resolve config-level default_timezone into date fields that don't specify their own
    if !config.admin.default_timezone.is_empty() {
        let default_tz = config.admin.default_timezone.clone();

        if let Ok(mut reg) = registry.write() {
            for def in reg.collections.values_mut() {
                apply_default_timezone(&mut def.fields, &default_tz);
            }
            for def in reg.globals.values_mut() {
                apply_default_timezone(&mut def.fields, &default_tz);
            }
        }
    }

    Ok(registry)
}

/// Recursively set `default_timezone` on Date fields with `timezone: true`
/// that don't already have their own default_timezone.
fn apply_default_timezone(fields: &mut [FieldDefinition], default_tz: &str) {
    for field in fields.iter_mut() {
        if field.field_type == FieldType::Date && field.timezone && field.default_timezone.is_none()
        {
            field.default_timezone = Some(default_tz.to_string());
        }

        // Recurse into composite fields
        apply_default_timezone(&mut field.fields, default_tz);
        for tab in &mut field.tabs {
            apply_default_timezone(&mut tab.fields, default_tz);
        }
    }
}

fn setup_package_paths(lua: &Lua, config_dir: &Path) -> Result<()> {
    let config_str = config_dir.to_string_lossy();
    let pkg: Table = lua.globals().get("package")?;
    let current_path: String = pkg.get("path")?;
    let new_path = format!("{0}/?.lua;{0}/?/init.lua;{1}", config_str, current_path);
    pkg.set("path", new_path)?;
    Ok(())
}

/// Apply sandbox restrictions to a Lua VM.
/// Re-adds the `os` library with only safe functions (time, date, clock, difftime)
/// and removes dangerous globals (loadfile, dofile).
pub(crate) fn sandbox_lua(lua: &Lua) -> Result<()> {
    // ALL_SAFE excludes `os`, `io`, `ffi`, and `debug`.
    // Re-add os with only safe functions.
    lua.load_std_libs(StdLib::OS)?;
    let os: Table = lua.globals().get("os")?;
    os.set("execute", mlua::Value::Nil)?;
    os.set("remove", mlua::Value::Nil)?;
    os.set("rename", mlua::Value::Nil)?;
    os.set("exit", mlua::Value::Nil)?;
    os.set("tmpname", mlua::Value::Nil)?;
    os.set("getenv", mlua::Value::Nil)?;
    os.set("setlocale", mlua::Value::Nil)?;
    // Keeps: os.time, os.date, os.clock, os.difftime

    // Remove remaining dangerous globals that could bypass the sandbox.
    // `load` and `loadstring` can compile arbitrary code, `loadfile`/`dofile` read from disk.
    lua.globals().set("load", mlua::Value::Nil)?;
    lua.globals().set("loadstring", mlua::Value::Nil)?;
    lua.globals().set("loadfile", mlua::Value::Nil)?;
    lua.globals().set("dofile", mlua::Value::Nil)?;

    Ok(())
}

/// Load and execute all `.lua` files in a directory (used for `collections/` and `globals/`).
/// Returns the number of files loaded.
pub(crate) fn load_lua_dir(lua: &Lua, dir: &Path, kind: &str) -> Result<usize> {
    let mut entries: Vec<_> = fs::read_dir(dir)
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

        let code = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        lua.load(&code)
            .set_name(path.to_string_lossy())
            .exec()
            .with_context(|| format!("Failed to execute {}", path.display()))?;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::{Lua, LuaOptions, StdLib, Value};

    fn sandboxed_lua() -> Lua {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).unwrap();
        sandbox_lua(&lua).unwrap();
        lua
    }

    #[test]
    fn sandbox_removes_load() {
        let lua = sandboxed_lua();
        let val: Value = lua.globals().get("load").unwrap();
        assert!(matches!(val, Value::Nil), "load() must be removed");
    }

    #[test]
    fn sandbox_removes_loadstring() {
        let lua = sandboxed_lua();
        let val: Value = lua.globals().get("loadstring").unwrap();
        assert!(matches!(val, Value::Nil), "loadstring() must be removed");
    }

    #[test]
    fn sandbox_removes_loadfile() {
        let lua = sandboxed_lua();
        let val: Value = lua.globals().get("loadfile").unwrap();
        assert!(matches!(val, Value::Nil), "loadfile() must be removed");
    }

    #[test]
    fn sandbox_removes_dofile() {
        let lua = sandboxed_lua();
        let val: Value = lua.globals().get("dofile").unwrap();
        assert!(matches!(val, Value::Nil), "dofile() must be removed");
    }

    #[test]
    fn sandbox_removes_os_execute() {
        let lua = sandboxed_lua();
        let result = lua.load("os.execute('echo hi')").exec();
        assert!(result.is_err(), "os.execute must be blocked");
    }

    #[test]
    fn sandbox_allows_os_time() {
        let lua = sandboxed_lua();
        let result: i64 = lua.load("return os.time()").eval().unwrap();
        assert!(result > 0);
    }

    #[test]
    fn sandbox_load_cannot_bypass() {
        let lua = sandboxed_lua();
        let result = lua.load("load('return 1')()").exec();
        assert!(
            result.is_err(),
            "load() must not be usable to bypass sandbox"
        );
    }
}
