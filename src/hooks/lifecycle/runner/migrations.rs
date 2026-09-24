//! `HookRunner` methods for running Lua migrations.

use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use mlua::{Function, Lua, Table};

use crate::{
    db::{DbConnection, DbPool},
    hooks::{HookRunner, LuaCrudInfra, init::chunk_name},
};

/// One Lua data migration to run: the file and the direction (`"up"` or
/// `"down"`) whose function to call.
#[derive(Debug, Clone, Copy)]
pub struct MigrationCall<'a> {
    pub path: &'a Path,
    pub direction: &'a str,
}

impl<'a> MigrationCall<'a> {
    /// A call of `direction` in the migration file at `path`.
    #[must_use]
    pub fn new(path: &'a Path, direction: &'a str) -> Self {
        Self { path, direction }
    }
}

/// Load the migration module and call its `direction` function.
fn call_migration(lua: &Lua, call: &MigrationCall<'_>, code: &str) -> Result<()> {
    let path = call.path;

    let module: Table = lua
        .load(code)
        .set_name(chunk_name(path))
        .eval()
        .with_context(|| format!("Failed to load migration {}", path.display()))?;

    let func: Function = module.get(call.direction).with_context(|| {
        format!(
            "Migration {} does not have a '{}' function",
            path.display(),
            call.direction
        )
    })?;

    func.call::<()>(())
        .with_context(|| format!("Migration {}.{}() failed", path.display(), call.direction))
}

impl HookRunner {
    /// Run a migration file (up or down direction) in its own write
    /// transaction. Loads the Lua file and calls `M.up()` or `M.down()` with
    /// CRUD access, then `record` on the same transaction (the caller's
    /// bookkeeping — recording or removing the applied migration), and
    /// commits.
    ///
    /// The migration's Lua CRUD runs in the full transaction scope (see
    /// `run_in_system_tx`): upload files of documents it hard-deletes are
    /// removed, the populate cache is cleared, and live events are published
    /// through `infra` only after the commit. A failing migration rolls back
    /// with every file still in place.
    ///
    /// # Errors
    ///
    /// Returns an error if the file can't be read, the Lua module can't be
    /// loaded, the migration function or `record` fails, or the transaction
    /// can't be opened or committed.
    pub fn run_migration(
        &self,
        call: &MigrationCall<'_>,
        pool: &DbPool,
        infra: Option<LuaCrudInfra>,
        record: impl FnOnce(&dyn DbConnection) -> Result<()>,
    ) -> Result<()> {
        let code = fs::read_to_string(call.path)
            .with_context(|| format!("Failed to read migration {}", call.path.display()))?;

        let label = format!("migration {}", call.path.display());

        self.run_in_system_tx(pool, infra, &label, |lua, conn| {
            call_migration(lua, call, &code)?;

            record(conn)
        })
    }
}
