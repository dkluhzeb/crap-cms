//! `make migration` — generate migration Lua files.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use chrono::Local;
use serde_json::json;

use crate::cli;
use crate::scaffold::render::render;

/// Create a new migration file at `<config_dir>/migrations/YYYYMMDDHHMMSS_name.lua`.
pub fn make_migration(config_dir: &Path, name: &str) -> Result<()> {
    validate_migration_name(name)?;

    let migrations_dir = config_dir.join("migrations");
    fs::create_dir_all(&migrations_dir).context("Failed to create migrations/ directory")?;

    let timestamp = Local::now().format("%Y%m%d%H%M%S");
    let filename = format!("{}_{}.lua", timestamp, name);
    let file_path = migrations_dir.join(&filename);

    let lua = render("migration", &json!({}))?;

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));

    Ok(())
}

/// Validate a migration name: lowercase, digits, underscores only.
fn validate_migration_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!(
            "Invalid migration name '{}' — use lowercase letters, digits, and underscores only",
            name
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_migration(tmp.path(), "add_categories").unwrap();

        let entries: Vec<_> = fs::read_dir(tmp.path().join("migrations"))
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);

        let filename = entries[0].file_name().to_string_lossy().to_string();
        assert!(filename.ends_with("_add_categories.lua"));
        assert!(filename.len() > 15);
    }

    #[test]
    fn content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_migration(tmp.path(), "seed_data").unwrap();

        let entry = fs::read_dir(tmp.path().join("migrations"))
            .unwrap()
            .filter_map(|e| e.ok())
            .next()
            .unwrap();
        let content = fs::read_to_string(entry.path()).unwrap();
        assert!(content.contains("function M.up()"));
        assert!(content.contains("function M.down()"));
        assert!(content.contains("return M"));
    }

    #[test]
    fn invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(make_migration(tmp.path(), "bad name").is_err());
        assert!(make_migration(tmp.path(), "bad-name").is_err());
        assert!(make_migration(tmp.path(), "BadName").is_err());
        assert!(make_migration(tmp.path(), "").is_err());
    }

    #[test]
    fn valid_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(make_migration(tmp.path(), "add_users").is_ok());
        assert!(make_migration(tmp.path(), "v2_schema").is_ok());
        assert!(make_migration(tmp.path(), "fix123").is_ok());
    }
}
