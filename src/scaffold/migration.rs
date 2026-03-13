//! `make migration` command — generate migration Lua files.

use anyhow::{Context as _, Result, bail};
use std::{fs, path::Path};

/// Create a new migration file at `<config_dir>/migrations/YYYYMMDDHHMMSS_name.lua`.
pub fn make_migration(config_dir: &Path, name: &str) -> Result<()> {
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

    let migrations_dir = config_dir.join("migrations");
    fs::create_dir_all(&migrations_dir).context("Failed to create migrations/ directory")?;

    let timestamp = chrono::Local::now().format("%Y%m%d%H%M%S");
    let filename = format!("{}_{}.lua", timestamp, name);
    let file_path = migrations_dir.join(&filename);

    let lua = r#"local M = {}

function M.up()
    -- TODO: implement migration
    -- crap.* API available (find, create, update, delete)
end

function M.down()
    -- TODO: implement rollback (best-effort)
end

return M
"#
    .to_string();

    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    println!("Created {}", file_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_make_migration_creates_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_migration(tmp.path(), "add_categories").unwrap();

        let migrations_dir = tmp.path().join("migrations");
        let entries: Vec<_> = fs::read_dir(&migrations_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let filename = entries[0].file_name().to_string_lossy().to_string();
        assert!(filename.ends_with("_add_categories.lua"));
        // Timestamp prefix: 14 digits
        assert!(filename.len() > 15);
    }

    #[test]
    fn test_make_migration_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_migration(tmp.path(), "seed_data").unwrap();

        let migrations_dir = tmp.path().join("migrations");
        let entry = fs::read_dir(&migrations_dir)
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
    fn test_make_migration_invalid_name() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Spaces
        assert!(make_migration(tmp.path(), "bad name").is_err());
        // Special chars
        assert!(make_migration(tmp.path(), "bad-name").is_err());
        // Uppercase
        assert!(make_migration(tmp.path(), "BadName").is_err());
        // Empty
        assert!(make_migration(tmp.path(), "").is_err());
    }

    #[test]
    fn test_make_migration_valid_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(make_migration(tmp.path(), "add_users").is_ok());
        assert!(make_migration(tmp.path(), "v2_schema").is_ok());
        assert!(make_migration(tmp.path(), "fix123").is_ok());
    }
}
