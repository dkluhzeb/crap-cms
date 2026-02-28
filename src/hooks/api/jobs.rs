//! `crap.jobs` namespace — job definition.

use anyhow::Result;
use mlua::{Lua, Table};

use crate::core::SharedRegistry;

use super::parse;

/// Register `crap.jobs` — job definition.
pub(super) fn register_jobs(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let jobs_table = lua.create_table()?;
    let reg_clone = registry.clone();
    let define_job = lua.create_function(move |_lua, (slug, config): (String, Table)| {
        let def = parse::parse_job_definition(&slug, &config)
            .map_err(|e| mlua::Error::RuntimeError(format!(
                "Failed to parse job '{}': {}", slug, e
            )))?;
        let mut reg = reg_clone.write()
            .map_err(|e| mlua::Error::RuntimeError(format!("Registry lock poisoned: {}", e)))?;
        reg.register_job(def);
        Ok(())
    })?;
    jobs_table.set("define", define_job)?;
    crap.set("jobs", jobs_table)?;
    Ok(())
}
