//! HookRunner methods for running Lua migrations.

use std::{fs, path::Path};

use anyhow::{Context as _, Result};

use crate::hooks::{HookRunner, lifecycle::types::TxContextGuard};

impl HookRunner {
    /// Run a migration file (up or down direction) within a transaction.
    /// Loads the Lua file, calls `M.up()` or `M.down()` with CRUD access.
    pub fn run_migration(
        &self,
        path: &Path,
        direction: &str,
        conn: &dyn crate::db::DbConnection,
    ) -> Result<()> {
        let code = fs::read_to_string(path)
            .with_context(|| format!("Failed to read migration {}", path.display()))?;

        let lua = self.pool.acquire()?;
        let _guard = TxContextGuard::set(&lua, conn, None, None);

        // Load the migration module
        let chunk = lua.load(&code).set_name(path.to_string_lossy());
        let module: mlua::Table = chunk
            .eval()
            .with_context(|| format!("Failed to load migration {}", path.display()))?;

        // Call M.up() or M.down()
        let func: mlua::Function = module.get(direction).with_context(|| {
            format!(
                "Migration {} does not have a '{}' function",
                path.display(),
                direction
            )
        })?;

        func.call::<()>(())
            .with_context(|| format!("Migration {}.{}() failed", path.display(), direction))?;

        Ok(())
    }
}
