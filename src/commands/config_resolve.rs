//! Auto-detect config directory by walking up from CWD looking for `crap.toml`.
//!
//! Resolution priority:
//! 1. `--config` / `-C` flag (explicit)
//! 2. `CRAP_CONFIG_DIR` env var (handled by clap `env = "..."`)
//! 3. Walk up from CWD looking for `crap.toml`

use anyhow::{Result, anyhow, bail};
use std::{
    env,
    path::{Path, PathBuf},
};

/// Resolve the config directory from an explicit path or by auto-detection.
///
/// If `explicit` is `Some` (from `--config` flag or `CRAP_CONFIG_DIR` env var via clap),
/// canonicalizes and validates that `crap.toml` exists there.
/// If `None`, walks up from `std::env::current_dir()` looking for `crap.toml`.
pub fn resolve_config_dir(explicit: Option<PathBuf>) -> Result<PathBuf> {
    match explicit {
        Some(path) => validate_config_dir(&path),
        None => find_config_in_ancestors(),
    }
}

/// Validate that an explicit path contains `crap.toml`.
fn validate_config_dir(path: &Path) -> Result<PathBuf> {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    if !resolved.join("crap.toml").exists() {
        bail!(
            "No crap.toml found in '{}'\n\n\
             Make sure the path points to a valid crap-cms config directory.",
            resolved.display()
        );
    }

    Ok(resolved)
}

/// Walk up from CWD looking for a directory containing `crap.toml`.
fn find_config_in_ancestors() -> Result<PathBuf> {
    let cwd = env::current_dir().map_err(|e| {
        anyhow!(
            "Failed to determine current directory: {}\n\n\
             Use --config <path> or set CRAP_CONFIG_DIR to specify the config directory.",
            e
        )
    })?;

    let mut dir = cwd.as_path();

    loop {
        if dir.join("crap.toml").exists() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    bail!(
        "No crap.toml found in '{}' or any parent directory.\n\n\
         To fix this, either:\n  \
         - Run from inside a crap-cms project directory\n  \
         - Use --config <path> to specify the config directory\n  \
         - Set CRAP_CONFIG_DIR environment variable",
        cwd.display()
    )
}

#[cfg(test)]
mod tests {
    use std::{env, fs, sync::Mutex};

    use super::*;

    /// Tests that change `std::env::current_dir()` must hold this mutex.
    /// CWD is process-global, so concurrent changes cause flaky failures
    /// (e.g., another test's tempdir gets cleaned up while CWD points to it).
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn explicit_path_used_directly() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();

        let result = resolve_config_dir(Some(tmp.path().to_path_buf()));
        assert!(result.is_ok());
        // Canonicalized path should end with the same dir name
        let resolved = result.unwrap();
        assert!(resolved.join("crap.toml").exists());
    }

    #[test]
    fn explicit_path_without_toml_fails() {
        let tmp = tempfile::tempdir().unwrap();

        let result = resolve_config_dir(Some(tmp.path().to_path_buf()));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("No crap.toml found"),
            "unexpected error: {}",
            msg
        );
    }

    #[test]
    fn walk_up_from_subdirectory_finds_parent() {
        let _guard = CWD_LOCK.lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        let subdir = tmp.path().join("collections").join("deep");
        fs::create_dir_all(&subdir).unwrap();

        let original_cwd = env::current_dir().unwrap();
        env::set_current_dir(&subdir).unwrap();

        let result = find_config_in_ancestors();
        env::set_current_dir(&original_cwd).unwrap();

        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert!(resolved.join("crap.toml").exists());
        assert_eq!(resolved, tmp.path().to_path_buf());
    }

    #[test]
    fn walk_up_from_config_dir_itself() {
        let _guard = CWD_LOCK.lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();

        let original_cwd = env::current_dir().unwrap();
        env::set_current_dir(tmp.path()).unwrap();

        let result = find_config_in_ancestors();
        env::set_current_dir(&original_cwd).unwrap();

        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert!(resolved.join("crap.toml").exists());
    }

    #[test]
    fn fails_when_no_toml_anywhere() {
        let _guard = CWD_LOCK.lock().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();

        let original_cwd = env::current_dir().unwrap();
        env::set_current_dir(&deep).unwrap();

        let result = find_config_in_ancestors();
        env::set_current_dir(&original_cwd).unwrap();

        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("No crap.toml found"),
            "unexpected error: {}",
            msg
        );
        assert!(
            msg.contains("--config"),
            "error should mention --config flag: {}",
            msg
        );
        assert!(
            msg.contains("CRAP_CONFIG_DIR"),
            "error should mention env var: {}",
            msg
        );
    }
}
