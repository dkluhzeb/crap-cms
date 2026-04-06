//! `crap.jobs` namespace — job definition.

use anyhow::Result;
use mlua::{Error::RuntimeError, Lua, Table};

use crate::{core::SharedRegistry, hooks::api::parse};

/// Register `crap.jobs.define` — job definition.
pub(super) fn register_jobs(lua: &Lua, crap: &Table, registry: SharedRegistry) -> Result<()> {
    let t = lua.create_table()?;

    let reg = registry.clone();
    t.set(
        "define",
        lua.create_function(move |_, (slug, config): (String, Table)| {
            define(&reg, &slug, &config)
        })?,
    )?;

    crap.set("jobs", t)?;

    Ok(())
}

/// Parse and register a job definition.
fn define(reg: &SharedRegistry, slug: &str, config: &Table) -> mlua::Result<()> {
    let def = parse::parse_job_definition(slug, config)
        .map_err(|e| RuntimeError(format!("Failed to parse job '{slug}': {e}")))?;

    reg.write()
        .map_err(|e| RuntimeError(format!("Registry lock poisoned: {e:#}")))?
        .register_job(def);

    Ok(())
}
