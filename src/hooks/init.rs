//! Lua VM initialization, sandboxing, and definition loading.

use std::{fs, path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use tracing::{debug, info};

use crate::{
    config::CrapConfig,
    core::{FieldDefinition, FieldType, Registry, SharedRegistry},
    hooks::lifecycle::InitPhase,
};

use super::lua_api;

/// Initialize the Lua VM, register the crap API, load collections/globals,
/// and run init.lua. Returns an immutable `Arc<Registry>` snapshot.
///
/// The writeable `SharedRegistry` exists only within this function's
/// stack. After all definitions are loaded and validations pass, the
/// registry is snapshotted and the writeable handle is dropped. Every
/// downstream consumer (`HookRunner`, `AdminState`, MCP, gRPC, scheduler)
/// holds only the `Arc<Registry>` snapshot.
///
/// # Errors
///
/// Returns an error if the Lua VM can't be created, sandboxing fails, the
/// `crap` global can't be registered, or any of the `<config_dir>/*.lua`
/// files fail to load.
pub fn init_lua(config_dir: &Path, config: &CrapConfig) -> Result<Arc<Registry>> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;

    sandbox_lua(&lua)?;

    lua.set_app_data(lua_api::VmLabel("init".to_string()));

    let registry = Registry::shared();

    setup_package_paths(&lua, config_dir)?;
    install_relative_chunk_searcher(&lua)?;
    lua_api::register_api(&lua, &registry, config)?;

    // Mark init phase so register-only APIs (`crap.pages.register`,
    // `crap.template_data.register`, …) accept calls. Cleared after
    // init.lua so any later runtime call gets a clear error.
    lua.set_app_data(InitPhase);

    let n_collections = load_def_dir(&lua, config_dir, "collection")?;
    let n_globals = load_def_dir(&lua, config_dir, "global")?;
    let n_jobs = load_def_dir(&lua, config_dir, "job")?;

    // Per-slug typing-helper factories — `crap.collections.<slug>.field_hook(...)`
    // etc. need to exist before init.lua / the hook-ref validation pass
    // tries to `require` any hook file that wraps in a factory. The init
    // VM gets only the typing helpers (no CRUD wrappers — those are
    // pool-VM only).
    lua_api::register::register_per_slug_typing_helpers(&lua, &registry)
        .context("Failed to register per-slug typing helpers")?;

    let has_init = execute_init_lua(&lua, config_dir)?;

    lua.remove_app_data::<InitPhase>();

    info!(
        "Lua init: loaded {} collection(s), {} global(s), {} job(s){}",
        n_collections,
        n_globals,
        n_jobs,
        if has_init { ", executed init.lua" } else { "" }
    );

    apply_config_defaults(&registry, config);

    // Take the snapshot before validations so they read from the
    // frozen view (and the rest of the function can pass `&Registry`
    // instead of `&SharedRegistry`).
    let snapshot = Registry::snapshot(&registry);

    // BUG-5: statically-known hook/access refs are resolved at startup so
    // typos fail to boot instead of surfacing at first request.
    super::startup_checks::validate_hook_references(&lua, &snapshot)
        .context("Hook/access reference validation failed")?;

    // Custom route handler/access refs must resolve and not collide — fail to
    // boot rather than 500 (or panic at router assembly) on first request.
    super::startup_checks::validate_routes(&lua, &config.routes.prefix)
        .context("Custom route validation failed")?;

    // The [admin] access gate ref must resolve — the runtime gate fails
    // closed, so a typo here would lock everyone out of the admin panel.
    super::startup_checks::validate_admin_access_ref(&lua, config.admin.access.as_ref())
        .context("Admin access gate validation failed")?;

    // Reject field names that collide with the generated locale-suffixed
    // column pattern `{name}__{locale}`.
    super::startup_checks::validate_locale_field_collisions(&snapshot, &config.locale.locales)
        .context("Locale/field-name collision detected")?;

    // Reject `required_locales` settings that reference unconfigured locales,
    // so a typo fails to boot instead of failing every non-draft write at
    // runtime with a confusing `validation.required_locale` error.
    super::startup_checks::validate_required_locales(&snapshot, &config.locale.locales)
        .context("Invalid required_locales configuration")?;

    // Reject definitions whose generated table names collide (e.g. a
    // collection slugged `posts_tags` vs the `tags` array field of `posts`).
    super::startup_checks::validate_table_name_collisions(&snapshot)
        .context("Table name collision detected")?;

    // Validate per-collection auth.methods configurations: hard errors
    // for structural issues (enabled+empty methods, duplicate password_login,
    // etc.), warnings for footgun patterns (always-active strategies).
    super::startup_checks::validate_auth_methods(&snapshot)
        .context("Auth method configuration invalid")?;

    // Advisory warning (not a hard error): with default_deny = false, a
    // collection's draft/trash view with no gating rule is world-readable.
    super::startup_checks::warn_public_lifecycle_views(&snapshot, config.access.default_deny);

    // The init VM and `registry` (SharedRegistry) drop here. The
    // closures inside the VM that captured SharedRegistry clones are
    // also dropped; no writeable handle survives this function.
    drop(lua);
    drop(registry);

    Ok(snapshot)
}

/// Execute init.lua if present. Returns whether it existed.
fn execute_init_lua(lua: &Lua, config_dir: &Path) -> Result<bool> {
    let init_path = config_dir.join("init.lua");

    if !init_path.exists() {
        return Ok(false);
    }

    debug!("[lua:init] Executing init.lua");

    let code = fs::read_to_string(&init_path)
        .with_context(|| format!("Failed to read {}", init_path.display()))?;

    lua.load(&code)
        .set_name(chunk_name(&init_path))
        .exec()
        .with_context(|| "Failed to execute init.lua")?;

    Ok(true)
}

/// Load definition files from `{config_dir}/{kind}s/` if the directory exists.
fn load_def_dir(lua: &Lua, config_dir: &Path, kind: &str) -> Result<usize> {
    let dir_name = format!("{kind}s");
    let dir = config_dir.join(&dir_name);

    if dir.exists() {
        // Pass the plural directory name as the require-key prefix so
        // `require("jobs.foo")` hits the cache populated here for
        // `<config_dir>/jobs/foo.lua`.
        load_lua_dir(lua, &dir, &dir_name)
    } else {
        Ok(0)
    }
}

/// Resolve config-level `default_timezone` into date fields that don't specify their own.
fn apply_config_defaults(registry: &SharedRegistry, config: &CrapConfig) {
    if config.admin.default_timezone.is_empty() {
        return;
    }

    let default_tz = &config.admin.default_timezone;

    let Ok(mut reg) = registry.write() else {
        return;
    };

    // Init phase: the registry is being built and nothing else holds a
    // reference to these Arcs yet, so `make_mut` mutates in place without
    // cloning.
    for def in reg.collections.values_mut() {
        apply_default_timezone(&mut Arc::make_mut(def).fields, default_tz);
    }
    for def in reg.globals.values_mut() {
        apply_default_timezone(&mut Arc::make_mut(def).fields, default_tz);
    }
}

/// Recursively set `default_timezone` on Date fields with `timezone: true`
/// that don't already have their own `default_timezone`.
fn apply_default_timezone(fields: &mut [FieldDefinition], default_tz: &str) {
    for field in fields.iter_mut() {
        if field.field_type == FieldType::Date && field.timezone && field.default_timezone.is_none()
        {
            field.default_timezone = Some(default_tz.to_string());
        }

        apply_default_timezone(&mut field.fields, default_tz);

        for tab in &mut field.tabs {
            apply_default_timezone(&mut tab.fields, default_tz);
        }

        // Blocks sub-fields live under `blocks[].fields`, not `fields` — a Date
        // with `timezone` nested inside a Blocks field must inherit the config
        // default like one nested in a group/array/tab.
        for block in &mut field.blocks {
            apply_default_timezone(&mut block.fields, default_tz);
        }
    }
}

/// Chunk name for Lua error messages: the last two path components
/// (`hooks/posts.lua`, `init.lua`) instead of the absolute server path.
/// Lua prefixes `error()` text with `chunkname:line:`, and hook errors
/// travel verbatim to API clients (gRPC `INVALID_ARGUMENT`, admin toasts,
/// MCP tool results) — an absolute name would disclose the server's
/// filesystem layout on every hook error.
pub(crate) fn chunk_name(path: &Path) -> String {
    let mut parts: Vec<&str> = path
        .iter()
        .rev()
        .take(2)
        .filter_map(|c| c.to_str())
        .collect();
    parts.reverse();
    parts.join("/")
}

fn setup_package_paths(lua: &Lua, config_dir: &Path) -> Result<()> {
    let config_str = config_dir.to_string_lossy();
    let pkg: Table = lua.globals().get("package")?;
    let current_path: String = pkg.get("path")?;
    let new_path = format!("{config_str}/?.lua;{config_str}/?/init.lua;{current_path}");

    pkg.set("path", new_path)?;

    Ok(())
}

/// Replace Lua's stock file searcher (`package.searchers[2]`) with one that
/// loads config-dir modules under a RELATIVE chunk name (`hooks/posts.lua`,
/// never the absolute `{config_dir}/hooks/posts.lua`).
///
/// The stock searcher names a loaded chunk by the exact `package.path` entry
/// it matched — an absolute path here — so a runtime `error()` inside a hook
/// file resolved via `require` surfaces that absolute server path in the
/// client-facing `HookError` message, disclosing the filesystem layout. This
/// searcher resolves modules identically (it walks the same `package.path`)
/// but stamps the shortened `chunk_name`, matching the `load_lua_dir` and
/// init.lua load paths. The preload searcher (index 1) still runs first, so
/// `load_lua_dir`-cached modules are untouched.
///
/// # Errors
///
/// Returns an error if `package.searchers` can't be read or the replacement
/// searcher can't be installed.
pub(crate) fn install_relative_chunk_searcher(lua: &Lua) -> Result<()> {
    let searcher = lua.create_function(|lua, module: String| {
        let package: Table = lua.globals().get("package")?;
        let path: String = package.get("path")?;
        let rel = module.replace('.', "/");

        for template in path.split(';') {
            if template.is_empty() {
                continue;
            }

            let candidate = template.replace('?', &rel);
            let file = Path::new(&candidate);

            let Ok(code) = fs::read_to_string(file) else {
                continue;
            };

            let loader = lua.load(&code).set_name(chunk_name(file)).into_function()?;

            return Ok(Value::Function(loader));
        }

        // No file matched — a string message tells `require` to keep trying
        // its remaining searchers (and feeds the aggregated not-found error).
        Ok(Value::String(lua.create_string(format!(
            "\n\tno file '{module}' under the configured package.path"
        ))?))
    })?;

    let package: Table = lua.globals().get("package")?;
    let searchers: Table = package.get("searchers")?;
    searchers.set(2, searcher)?;

    Ok(())
}

/// Apply sandbox restrictions to a Lua VM.
///
/// The capability contract (pinned by
/// `sandbox_globals_match_reviewed_allowlist`):
/// - no process execution (`os.execute` AND its sibling `io.popen`),
/// - no dynamic code loading (`load`/`loadstring`/`loadfile`/`dofile`),
/// - no native module loading (`package.cpath`/`loadlib`, `string.dump`),
/// - `os` reduced to time functions,
/// - `io` file access stays available **deliberately** — custom storage
///   backends are documented as Lua-may-map-to-filesystem.
pub(crate) fn sandbox_lua(lua: &Lua) -> Result<()> {
    lua.load_std_libs(StdLib::OS)?;

    let os: Table = lua.globals().get("os")?;
    os.set("execute", Value::Nil)?;
    os.set("remove", Value::Nil)?;
    os.set("rename", Value::Nil)?;
    os.set("exit", Value::Nil)?;
    os.set("tmpname", Value::Nil)?;
    os.set("getenv", Value::Nil)?;
    os.set("setlocale", Value::Nil)?;

    lua.globals().set("load", Value::Nil)?;
    lua.globals().set("loadstring", Value::Nil)?;
    lua.globals().set("loadfile", Value::Nil)?;
    lua.globals().set("dofile", Value::Nil)?;

    let pkg: Table = lua.globals().get("package")?;
    pkg.set("cpath", "")?;
    pkg.set("loadlib", Value::Nil)?;

    let string_table: Table = lua.globals().get("string")?;
    string_table.set("dump", Value::Nil)?;

    // `io.popen` is process execution — the sibling of `os.execute`,
    // which this sandbox has always removed. (The rest of `io` stays:
    // custom storage backends legitimately touch the filesystem.)
    let io: Table = lua.globals().get("io")?;
    io.set("popen", Value::Nil)?;

    Ok(())
}

/// Load and execute all `.lua` files in a directory (used for
/// `collections/`, `globals/`, `jobs/`).
///
/// Each file is evaluated and its return value is cached in
/// `package.loaded["<kind>.<stem>"]` so subsequent `require()`
/// calls (e.g. the job dispatcher resolving a handler) hit the
/// cache and don't re-execute the file's top-level. Without this,
/// a `crap.<x>.define(...)` call at the top of `jobs/foo.lua`
/// would run again at runtime and trip the `InitPhase` guard.
/// Files that don't `return` cache as `true` (Lua's standard
/// require-no-return convention).
pub(crate) fn load_lua_dir(lua: &Lua, dir: &Path, kind: &str) -> Result<usize> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("Failed to read {} directory: {}", kind, dir.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "lua"))
        .collect();

    entries.sort_by_key(std::fs::DirEntry::file_name);

    let pkg: Table = lua.globals().get("package")?;
    let loaded: Table = pkg.get("loaded")?;

    let count = entries.len();
    for entry in entries {
        let path = entry.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        let name = name.to_string_lossy();
        let label = lua
            .app_data_ref::<lua_api::VmLabel>()
            .map_or_else(|| "lua".into(), |l| l.0.clone());
        debug!("[lua:{label}] Loading {kind}: {name}");

        let code = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;

        let returned: Value = lua
            .load(&code)
            .set_name(chunk_name(&path))
            .eval()
            .with_context(|| format!("Failed to execute {}", path.display()))?;

        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let module_name = format!("{kind}.{stem}");
        let cached = match returned {
            Value::Nil => Value::Boolean(true),
            v => v,
        };
        loaded
            .set(module_name.as_str(), cached)
            .with_context(|| format!("Failed to cache loaded module '{module_name}'"))?;
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

    /// the sandbox is a denylist, and denylists
    /// rot ("removed `loadfile`/`dofile` but not `load`"; "removed
    /// `os.execute` but not `io.popen`" — both really happened). This
    /// pins the COMPLETE reviewed capability surface: a new global
    /// appearing (an mlua upgrade, a stdlib change) or a removed one
    /// resurfacing fails here and forces the review decision.
    #[test]
    fn sandbox_globals_match_reviewed_allowlist() {
        let lua = sandboxed_lua();

        let collect = |table: &mlua::Table| -> Vec<String> {
            let mut names: Vec<String> = table
                .pairs::<String, Value>()
                .filter_map(|p| p.ok().map(|(k, _)| k))
                .collect();
            names.sort();
            names
        };

        let globals = collect(&lua.globals());
        assert_eq!(
            globals,
            [
                "_G",
                "_VERSION",
                "assert",
                "collectgarbage",
                "coroutine",
                "error",
                "getmetatable",
                "io",
                "ipairs",
                "math",
                "next",
                "os",
                "package",
                "pairs",
                "pcall",
                "print",
                "rawequal",
                "rawget",
                "rawlen",
                "rawset",
                "require",
                "select",
                "setmetatable",
                "string",
                "table",
                "tonumber",
                "tostring",
                "type",
                "utf8",
                "warn",
                "xpcall",
            ],
            "sandboxed global set changed — review the new/removed capability"
        );

        let os: mlua::Table = lua.globals().get("os").unwrap();
        assert_eq!(collect(&os), ["clock", "date", "difftime", "time"]);

        let io: mlua::Table = lua.globals().get("io").unwrap();
        assert_eq!(
            collect(&io),
            [
                "close", "flush", "input", "lines", "open", "output", "read", "stderr", "stdin",
                "stdout", "tmpfile", "type", "write",
            ],
            "io capability set changed — `popen` must never return"
        );

        let string_table: mlua::Table = lua.globals().get("string").unwrap();
        assert!(
            !collect(&string_table).contains(&"dump".to_string()),
            "string.dump must stay removed"
        );
    }

    /// `io.popen` is process execution — `os.execute`'s sibling — and
    /// must be unreachable from hook code.
    #[test]
    fn sandbox_removes_io_popen() {
        let lua = sandboxed_lua();
        let result: Value = lua.load("return io.popen").eval().unwrap();
        assert!(matches!(result, Value::Nil), "io.popen must be nil");

        let err = lua.load(r#"io.popen("echo pwned")"#).exec();
        assert!(err.is_err(), "calling io.popen must fail");
    }

    /// Chunk names are config-dir-relative, so the `chunkname:line:`
    /// prefix Lua puts on `error()` text (which travels verbatim to API
    /// clients) names `hooks/x.lua`, never `/srv/app/hooks/x.lua`.
    #[test]
    fn chunk_names_are_relative_not_absolute() {
        assert_eq!(
            chunk_name(Path::new("/srv/app/config/hooks/posts.lua")),
            "hooks/posts.lua"
        );
        assert_eq!(
            chunk_name(Path::new("/srv/app/config/init.lua")),
            "config/init.lua"
        );
        assert_eq!(chunk_name(Path::new("init.lua")), "init.lua");
    }

    /// End-to-end: a Lua `error()` raised from loaded code carries the
    /// relative chunk name in its message, not the absolute path.
    #[test]
    fn lua_error_text_carries_relative_chunk_name() {
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("secret-dir");
        std::fs::create_dir_all(&deep).unwrap();
        let file = deep.join("boom.lua");
        std::fs::write(&file, "error('kaboom')").unwrap();

        let lua = sandboxed_lua();
        let code = std::fs::read_to_string(&file).unwrap();
        let err = lua
            .load(&code)
            .set_name(chunk_name(&file))
            .exec()
            .unwrap_err()
            .to_string();

        assert!(err.contains("secret-dir/boom.lua"), "chunk visible: {err}");
        assert!(
            !err.contains(&tmp.path().to_string_lossy().to_string()),
            "absolute path must not leak: {err}"
        );
    }

    /// Regression: hook files resolved at runtime via `require` (the
    /// file-per-hook and module patterns in `resolve_hook_function`) were
    /// loaded by Lua's stock searcher, which names the chunk by the ABSOLUTE
    /// `package.path` entry it matched — leaking the server's filesystem
    /// layout into the client-facing `HookError` text. `install_relative_chunk_searcher`
    /// stamps the relative `chunk_name` instead. Loading `collections/`,
    /// `globals/`, `jobs/`, and init.lua already named their chunks relatively;
    /// this closes the `hooks/` `require` gap so every load path agrees.
    #[test]
    fn required_module_error_carries_relative_chunk_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hooks_dir = tmp.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(hooks_dir.join("boom.lua"), "error('kaboom')").unwrap();

        let lua = sandboxed_lua();
        setup_package_paths(&lua, tmp.path()).unwrap();
        install_relative_chunk_searcher(&lua).unwrap();

        let err = lua
            .load("require('hooks.boom')")
            .exec()
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("hooks/boom.lua"),
            "relative chunk name expected in require error: {err}"
        );
        assert!(
            !err.contains(&*tmp.path().to_string_lossy()),
            "absolute path must not leak through require: {err}"
        );
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
    fn sandbox_removes_package_cpath() {
        let lua = sandboxed_lua();
        let result: String = lua.load("return package.cpath").eval().unwrap();
        assert_eq!(result, "", "package.cpath must be empty string");
    }

    #[test]
    fn sandbox_removes_package_loadlib() {
        let lua = sandboxed_lua();
        let val: Value = lua.load("return package.loadlib").eval().unwrap();
        assert!(matches!(val, Value::Nil), "package.loadlib must be nil");
    }

    #[test]
    fn sandbox_removes_string_dump() {
        let lua = sandboxed_lua();
        let val: Value = lua.load("return string.dump").eval().unwrap();
        assert!(matches!(val, Value::Nil), "string.dump must be nil");
    }

    #[test]
    fn sandbox_removes_os_execute_via_value() {
        let lua = sandboxed_lua();
        let val: Value = lua.load("return os.execute").eval().unwrap();
        assert!(matches!(val, Value::Nil), "os.execute must be nil");
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

    #[test]
    fn default_timezone_applies_to_date_inside_blocks() {
        use crate::core::BlockDefinition;

        let mut fields = vec![
            FieldDefinition::builder("body", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "event",
                    vec![
                        FieldDefinition::builder("starts_at", FieldType::Date)
                            .timezone(true)
                            .build(),
                    ],
                )])
                .build(),
        ];

        apply_default_timezone(&mut fields, "America/New_York");

        let date = &fields[0].blocks[0].fields[0];
        assert_eq!(
            date.default_timezone.as_deref(),
            Some("America/New_York"),
            "a timezone Date nested in a Blocks field must inherit the config default"
        );
    }
}
