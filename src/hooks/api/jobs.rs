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
    // reads from `SharedRegistry` once and never re-scans. A runtime
    // registration silently fails to enroll the cron entry, and the
    // queue worker never picks up the handler. Refuse explicitly.
    if lua.app_data_ref::<InitPhase>().is_none() {
        return Err(RuntimeError(
            "crap.jobs.define must be called from a definition file or init.lua \
             — runtime registration does not enroll the scheduler or queue worker"
                .into(),
        ));
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

    /// Regression: `crap.jobs.define` from a runtime hook must be
    /// rejected — same reasoning as `crap.collections.define`. The
    /// scheduler enrolls jobs once at startup; a runtime call lands in
    /// `SharedRegistry` but the scheduler never picks it up.
    #[test]
    fn define_outside_init_phase_is_rejected() {
        let lua = Lua::new();
        let crap = lua.create_table().unwrap();
        let registry: SharedRegistry = Arc::new(RwLock::new(Registry::new()));
        register_jobs(&lua, &crap, registry.clone()).unwrap();
        lua.globals().set("crap", crap).unwrap();
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
}
