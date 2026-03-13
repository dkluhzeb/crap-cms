//! HookRunner methods for running Lua migrations.

use std::{fs, path::Path};

use anyhow::{Context as _, Result};

use crate::hooks::lifecycle::types::{TxContext, UserContext};

use super::HookRunner;

impl HookRunner {
    /// Run a migration file (up or down direction) within a transaction.
    /// Loads the Lua file, calls `M.up()` or `M.down()` with CRUD access.
    pub fn run_migration(
        &self,
        path: &Path,
        direction: &str,
        conn: &rusqlite::Connection,
    ) -> Result<()> {
        let code = fs::read_to_string(path)
            .with_context(|| format!("Failed to read migration {}", path.display()))?;

        let lua = self.pool.acquire()?;

        // Inject connection for CRUD access
        lua.set_app_data(TxContext(conn as *const _));
        lua.set_app_data(UserContext(None));

        let result = (|| -> Result<()> {
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
        })();

        lua.remove_app_data::<TxContext>();
        lua.remove_app_data::<UserContext>();

        result
    }
}
