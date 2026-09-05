//! `db console` subcommand: open an interactive database shell.

use std::{path::Path, process};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::{
    cli,
    config::CrapConfig,
    db::{DbConnection, pool},
};

/// Open an interactive database console.
///
/// # Errors
///
/// Returns an error if config loading or pool creation fails, or the
/// console subprocess exits with a non-zero status.
#[cfg(not(tarpaulin_include))]
pub fn console(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let p = pool::create_pool(&config_dir, &cfg).context("Failed to create pool")?;
    let conn = p.get().context("Failed to get connection")?;

    let db_path = cfg.db_path(&config_dir);
    let mut cmd = console_command(
        conn.kind(),
        &db_path,
        cfg.database.url.as_ref().map(crate::config::DbUrl::as_str),
    )?;
    let program = cmd.get_program().to_string_lossy().into_owned();

    cli::info(&format!("Opening {} console ({program})", conn.kind()));

    let status = cmd
        .status()
        .with_context(|| format!("Failed to launch {program} — is it installed?"))?;

    if !status.success() {
        bail!("{program} exited with status {status}");
    }

    Ok(())
}

/// Build the shell command for the backend: `sqlite3 <path>` for `SQLite`,
/// `psql <url>` for `PostgreSQL` (`database.url` is a libpq conninfo string
/// or URI, which `psql` accepts verbatim as its first argument).
fn console_command(kind: &str, db_path: &Path, pg_url: Option<&str>) -> Result<process::Command> {
    match kind {
        "sqlite" => {
            if !db_path.exists() {
                bail!("Database file not found: {}", db_path.display());
            }

            let mut cmd = process::Command::new("sqlite3");
            cmd.arg(db_path);

            Ok(cmd)
        }
        "postgres" => {
            let url = pg_url
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .ok_or_else(|| anyhow!("database.url must be set to open a PostgreSQL console"))?;

            let mut cmd = process::Command::new("psql");
            cmd.arg(url);

            Ok(cmd)
        }
        other => bail!("No interactive console available for '{other}' backend"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn sqlite_console_launches_sqlite3_on_the_db_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let cmd = console_command("sqlite", file.path(), None).unwrap();
        assert_eq!(cmd.get_program(), "sqlite3");
        assert_eq!(args(&cmd), vec![file.path().to_string_lossy().into_owned()]);
    }

    #[test]
    fn sqlite_console_requires_existing_file() {
        let err = console_command("sqlite", Path::new("/nonexistent/crap.db"), None).unwrap_err();
        assert!(err.to_string().contains("Database file not found"), "{err}");
    }

    /// Regression: `db console` used to bail on Postgres; it now launches
    /// `psql` with the configured conninfo.
    #[test]
    fn postgres_console_launches_psql_with_url() {
        let cmd = console_command(
            "postgres",
            Path::new("unused"),
            Some(" host=db user=crap dbname=crap_cms "),
        )
        .unwrap();
        assert_eq!(cmd.get_program(), "psql");
        assert_eq!(args(&cmd), vec!["host=db user=crap dbname=crap_cms"]);
    }

    #[test]
    fn postgres_console_requires_url() {
        for url in [None, Some(""), Some("   ")] {
            let err = console_command("postgres", Path::new("unused"), url).unwrap_err();
            assert!(err.to_string().contains("database.url"), "{err}");
        }
    }

    #[test]
    fn unknown_backend_errors() {
        let err = console_command("mysql", Path::new("unused"), None).unwrap_err();
        assert!(err.to_string().contains("No interactive console"), "{err}");
    }
}
