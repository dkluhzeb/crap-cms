//! `crap.jobs` namespace — job definition.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{
    core::SharedRegistry,
    hooks::{api::parse, lifecycle::InitPhase},
};

/// Register `crap.jobs.define` — job definition.
pub(super) fn register_jobs(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let t = lua.create_table()?;

    let reg = registry.clone();
    t.set(
        "define",
        lua.create_function(move |lua, (slug, config): (String, Table)| {
            define(lua, &reg, &slug, &config)
        })?,
    )?;

    crap.set("jobs", t)?;

    Ok(())
}

/// Parse and register a job definition.
fn define(lua: &Lua, reg: &SharedRegistry, slug: &str, config: &Table) -> mlua::Result<()> {
    // Job definitions wire into the scheduler at startup — the scheduler
    // reads from `SharedRegistry` once and never re-scans. A NEW runtime
    // registration silently fails to enroll the cron entry, and the
    // queue worker never picks up the handler. Refuse those.
    //
    // Re-defining an EXISTING slug at runtime is allowed — matches the
    // `crap.collections.define` / `crap.globals.define` round-trip
    // pattern, and supports the common Lua plugin layout that mixes
    // `crap.jobs.define(...)` calls and handler functions in a single
    // `jobs/foo.lua` file (the job dispatcher `require`s it at runtime
    // to call the handler, re-executing the top-level define). The
    // registry update lands; the scheduler enrollment stays as it was.
    if lua.app_data_ref::<InitPhase>().is_none() {
        let already_registered = reg
            .read()
            .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
            .get_job(slug)
            .is_some();

        if !already_registered {
            return Err(RuntimeError(
                "crap.jobs.define must be called from a definition file or init.lua \
                 for a NEW job — runtime registration does not enroll the scheduler \
                 or queue worker. Re-defining an already-registered job is allowed."
                    .into(),
            ));
        }
    }

    let def = parse::parse_job_definition(slug, config)
        .map_err(|e| RuntimeError(format!("Failed to parse job '{slug}': {e}")))?;

    reg.write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
        .register_job(def);

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
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_jobs(&lua, &crap, registry.clone()).unwrap();
        lua.globals().set("crap", crap).unwrap();
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
        // Note: NO `set_app_data(InitPhase)` — simulating a runtime hook.

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

    /// Regression: `crap.jobs.define` for an ALREADY-registered slug must
    /// succeed outside the init phase, not error. Two motivating use
    /// cases collapse to the same code path:
    ///
    /// 1. Lua plugins commonly mix definitions and handlers in a single
    ///    file (`jobs/foo.lua`: `crap.jobs.define(...)` at the top,
    ///    handler functions in the returned module table). The job
    ///    dispatcher `require`s that file at runtime to call the
    ///    handler, which re-executes the top-level `define`. With the
    ///    strict guard, the `tests/jobs.rs` `test_job.lua` fixture
    ///    panics on every dispatcher invocation.
    ///
    /// 2. The documented `config.get → modify → define` round-trip
    ///    that `crap.collections.define` / `crap.globals.define` also
    ///    support — the registry update lands, scheduler enrollment
    ///    stays as it was at startup.
    ///
    /// The strict guard still fires for a NEW slug, since runtime
    /// registration of a previously-unknown job genuinely doesn't
    /// enroll the scheduler.
    #[test]
    fn redefine_already_registered_at_runtime_is_allowed() {
        let (lua, registry) = lua_with_jobs();

        // Phase 1: register a job under InitPhase (the canonical path).
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

        // Phase 2: redefine with new fields (`retries = 5`) outside the
        // init phase — simulates both the dispatcher's `require()` and
        // the round-trip pattern.
        lua.load(
            r#"
            crap.jobs.define("send_email", {
              handler = "jobs.email.send",
              retries = 5,
              timeout = 30,
            })
        "#,
        )
        .exec()
        .expect("runtime re-define of an already-registered job must succeed");

        // The new fields are reflected in the registry.
        let reg = registry.read().unwrap();
        let def = reg.get_job("send_email").expect("still registered");
        assert_eq!(def.handler, "jobs.email.send");
        assert_eq!(
            def.retries, 5,
            "redefine must update the registry entry, not no-op"
        );
    }
}
