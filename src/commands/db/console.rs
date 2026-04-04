//! `db console` subcommand: open an interactive database shell.

use std::{path::Path, process};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli,
    config::CrapConfig,
    db::{DbConnection, pool},
};

/// Open an interactive database console.
#[cfg(not(tarpaulin_include))]
pub fn console(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let p = pool::create_pool(&config_dir, &cfg).context("Failed to create pool")?;
    let conn = p.get().context("Failed to get connection")?;

    match conn.kind() {
        "sqlite" => {
            let db_path = cfg.db_path(&config_dir);

            if !db_path.exists() {
                bail!("Database file not found: {}", db_path.display());
            }

            cli::info(&format!("Opening SQLite console: {}", db_path.display()));

            let status = process::Command::new("sqlite3")
                .arg(&db_path)
                .status()
                .context("Failed to launch sqlite3 — is it installed?")?;

            if !status.success() {
                bail!("sqlite3 exited with status {}", status);
            }
        }
        other => bail!("No interactive console available for '{}' backend", other),
    }

    Ok(())
}
