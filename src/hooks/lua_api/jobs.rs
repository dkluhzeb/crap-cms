//! `crap.jobs` namespace — job definition (init only at runtime).

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Result as LuaResult};

use crate::core::{Registry, SharedRegistry};
use crate::hooks::{
    lifecycle::InitPhase,
    lua_api::parse::{self, JobDefinitionConfig},
};
use crate::typegen::lua::{LuaFnSpec, LuaParam, lua_fn, lua_table};
use std::sync::Arc;

const DEFINE_INIT_ONLY_ERROR: &str = "crap.jobs.define must be called from a definition file or \
     init.lua. To change a registered job, edit the file and restart the process.";

/// Define a background job. Call in init.lua or jobs/*.lua files.
/// The handler function receives a context table with `data` and `job` fields,
/// and has full CRUD access (crap.collections.find/create/update/delete).
#[lua_fn(path = "crap.jobs.define")]
fn jobs_define_init(
    state: &SharedRegistry,
    lua: &Lua,
    #[lua(doc = "Unique job identifier.")] slug: String,
    #[lua(ty = "crap.JobDefinitionConfig", doc = "Job configuration.")] config: JobDefinitionConfig,
) -> LuaResult<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(DEFINE_INIT_ONLY_ERROR.into()));
    }

    let def = parse::parse_job_definition(&slug, config)
        .map_err(|e| RuntimeError(format!("Failed to parse job '{slug}': {e}")))?;

    state
        .write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
        .register_job(def);

    Ok(())
}

/// Pool-VM stub of `crap.jobs.define`. Same `InitPhase` guard as the init
/// variant — pool VMs re-run `jobs/*.lua` (to populate per-VM handler
/// closures) but the `define` call lands here as a no-op since the
/// `init_lua` VM already wrote the job to the shared registry.
#[lua_fn(path = "crap.jobs.define")]
fn jobs_define_pool(
    _state: &(),
    lua: &Lua,
    _slug: String,
    #[lua(ty = "crap.JobDefinitionConfig")] _config: JobDefinitionConfig,
) -> LuaResult<()> {
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(DEFINE_INIT_ONLY_ERROR.into()));
    }
    Ok(())
}

lua_table! {
    name: crap_jobs_init,
    path: "crap.jobs",
    state: SharedRegistry,
    header: "Background job definition and queuing API.",
    fns: [jobs_define_init],
}

lua_table! {
    name: crap_jobs_pool,
    path: "crap.jobs",
    state: (),
    fns: [jobs_define_pool],
}

/// Init-time registration: registers write-capable `crap.jobs.define`.
/// The common Lua plugin layout that mixes `crap.jobs.define(...)` and
/// handler functions in a single `jobs/foo.lua` file is supported via
/// `package.loaded` caching in `init::load_lua_dir`: the file's
/// top-level runs exactly once at boot, and the dispatcher's later
/// `require("jobs.foo")` hits the cache instead of re-evaluating.
pub(super) fn register_jobs_init(lua: &Lua, registry: SharedRegistry) -> Result<()> {
    register_crap_jobs_init(lua, registry)?;
    Ok(())
}

/// Pool-VM registration: `define` is a no-op stub. Pool VMs DO re-run
/// `jobs/*.lua` (handler functions live there as Lua module returns),
/// but the top-level `crap.jobs.define` call lands here as a no-op
/// since the `init_lua` VM already wrote the job to the registry. The
/// handler module return (`return M`) still produces per-VM function
/// handles via `require`.
pub(super) fn register_jobs_pool_init(lua: &Lua, _registry: Arc<Registry>) -> Result<()> {
    register_crap_jobs_pool(lua, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Registry;
    use std::sync::{Arc, RwLock};

    /// Set up a Lua VM with `crap.jobs` registered against a fresh
    /// `SharedRegistry`. Common helper for the two test cases below.
    fn lua_with_jobs() -> (Lua, SharedRegistry) {
        let lua = Lua::new();
        lua.globals()
            .set("crap", lua.create_table().unwrap())
            .unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_jobs_init(&lua, Arc::clone(&registry)).unwrap();
        (lua, registry)
    }

    /// Regression: `crap.jobs.define` from a runtime hook with a NEW slug
    /// must be rejected — same reasoning as `crap.collections.define`.
    /// The scheduler enrolls jobs once at startup; a runtime registration
    /// lands in `SharedRegistry` but the scheduler never picks it up, so
    /// the queue worker silently never invokes the handler.
    #[test]
    fn define_outside_init_phase_is_rejected() {
        let (lua, registry) = lua_with_jobs();

        let err = lua
            .load(r#"crap.jobs.define("send_email", {})"#)
            .exec()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("definition file") || err.contains("runtime registration"),
            "expected init-only error message, got: {err}"
        );

        let reg = registry.read().unwrap();
        assert!(
            reg.get_job("send_email").is_none(),
            "job must NOT be registered when call is refused",
        );
    }

    /// `crap.jobs.define` is rejected at runtime for both new and existing
    /// slugs. The "mix definition + handler in jobs/foo.lua" plugin pattern
    /// is supported via `package.loaded` caching in `init::load_lua_dir`:
    /// the file's top-level (including the `define` call) runs exactly once
    /// at boot with `InitPhase` set, and the dispatcher's later
    /// `require("jobs.foo")` hits the cache without re-evaluating.
    #[test]
    fn runtime_define_rejected_for_existing_slug() {
        let (lua, registry) = lua_with_jobs();

        lua.set_app_data(InitPhase);
        lua.load(
            r#"
            crap.jobs.define("send_email", {
              handler = "jobs.email.send",
              retries = 1,
              timeout = 30,
            })
        "#,
        )
        .exec()
        .expect("init-time define should succeed");
        lua.remove_app_data::<InitPhase>();

        let err = lua
            .load(
                r#"
                crap.jobs.define("send_email", {
                  handler = "jobs.email.send",
                  retries = 5,
                  timeout = 30,
                })
            "#,
            )
            .exec()
            .expect_err("runtime define for existing slug must be rejected");
        assert!(
            err.to_string().contains("init.lua"),
            "error should mention init.lua: {err}"
        );

        let reg = registry.read().unwrap();
        let def = reg.get_job("send_email").expect("still registered");
        assert_eq!(
            def.retries,
            Some(1),
            "registry entry untouched by rejected call"
        );
    }
}
