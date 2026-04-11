//! Shared helpers for blueprint operations — filesystem, validation, paths.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};

/// Files and directories to skip when saving a blueprint (runtime artifacts).
pub(super) const BLUEPRINT_SKIP: &[&str] = &["data", "uploads", "types"];

/// Resolve the global blueprints directory.
///
/// - Linux: `~/.config/crap-cms/blueprints/`
/// - macOS: `~/Library/Application Support/crap-cms/blueprints/`
/// - Windows: `C:\Users\<user>\AppData\Roaming\crap-cms\blueprints\`
pub(super) fn blueprints_dir() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| anyhow!("Could not determine config directory for your platform"))?;

    Ok(base.join("crap-cms").join("blueprints"))
}

/// Recursively copy a directory, skipping entries whose names match `skip`.
pub(super) fn copy_dir_recursive(src: &Path, dst: &Path, skip: &[&str]) -> Result<()> {
    for entry in fs::read_dir(src)
        .with_context(|| format!("Failed to read directory '{}'", src.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if skip.iter().any(|s| *s == name_str.as_ref()) {
            continue;
        }

        let src_path = entry.path();
        let dst_path = dst.join(&name);

        if src_path.is_dir() {
            fs::create_dir_all(&dst_path)?;
            copy_dir_recursive(&src_path, &dst_path, &[])?; // skip only applies at top level
        } else {
            fs::copy(&src_path, &dst_path)
                .with_context(|| format!("Failed to copy '{}'", src_path.display()))?;
        }
    }

    Ok(())
}

/// Count `.lua` files in a directory (0 if directory doesn't exist).
pub(super) fn count_lua_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map(|ext| ext == "lua")
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Validate a blueprint name: alphanumeric, hyphens, underscores.
pub(super) fn validate_blueprint_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("Blueprint name cannot be empty");
    }

    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        bail!(
            "Invalid blueprint name '{}' — use alphanumeric characters, hyphens, and underscores only",
            name
        );
    }

    Ok(())
}

/// Mutex to serialize tests that mutate XDG_CONFIG_HOME.
#[cfg(test)]
pub(super) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run a closure with XDG_CONFIG_HOME set to a temp path, then restore the original value.
#[cfg(test)]
pub(super) fn with_temp_config_dir<F>(f: F)
where
    F: FnOnce(&Path),
{
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let orig = std::env::var("XDG_CONFIG_HOME").ok();

    unsafe { std::env::set_var("XDG_CONFIG_HOME", tmp.path()) };
    f(tmp.path());

    match orig {
        Some(v) => unsafe { std::env::set_var("XDG_CONFIG_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_valid() {
        assert!(validate_blueprint_name("blog").is_ok());
        assert!(validate_blueprint_name("my-blog").is_ok());
        assert!(validate_blueprint_name("blog_v2").is_ok());
        assert!(validate_blueprint_name("abc123").is_ok());
        assert!(validate_blueprint_name("my_blog_v2").is_ok());
        assert!(validate_blueprint_name("A-B-C").is_ok());
    }

    #[test]
    fn validate_name_invalid() {
        assert!(validate_blueprint_name("").is_err());
        assert!(validate_blueprint_name("bad name").is_err());
        assert!(validate_blueprint_name("bad/name").is_err());
        assert!(validate_blueprint_name("a.b").is_err());
        assert!(validate_blueprint_name("a\\b").is_err());
        assert!(validate_blueprint_name("a@b").is_err());
    }

    #[test]
    fn copy_dir_recursive_basic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("collections")).unwrap();
        fs::create_dir_all(src.join("data")).unwrap();
        fs::write(src.join("crap.toml"), "# config").unwrap();
        fs::write(src.join("collections/posts.lua"), "-- posts").unwrap();
        fs::write(src.join("data/crap.db"), "binary").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, &["data"]).unwrap();

        assert!(dst.join("crap.toml").exists());
        assert!(dst.join("collections/posts.lua").exists());
        assert!(!dst.join("data").exists(), "data/ should be skipped");
    }

    #[test]
    fn copy_dir_recursive_nested_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("a/b/c")).unwrap();
        fs::write(src.join("a/b/c/deep.txt"), "deep content").unwrap();
        fs::write(src.join("a/top.txt"), "top content").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, &[]).unwrap();

        assert_eq!(
            fs::read_to_string(dst.join("a/b/c/deep.txt")).unwrap(),
            "deep content"
        );
        assert_eq!(
            fs::read_to_string(dst.join("a/top.txt")).unwrap(),
            "top content"
        );
    }

    #[test]
    fn copy_dir_recursive_skip_multiple() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("data")).unwrap();
        fs::create_dir_all(src.join("uploads")).unwrap();
        fs::create_dir_all(src.join("types")).unwrap();
        fs::create_dir_all(src.join("collections")).unwrap();
        fs::write(src.join("crap.toml"), "# config").unwrap();
        fs::write(src.join("data/crap.db"), "db").unwrap();
        fs::write(src.join("uploads/photo.jpg"), "photo").unwrap();
        fs::write(src.join("types/crap.lua"), "types").unwrap();
        fs::write(src.join("collections/posts.lua"), "posts").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, BLUEPRINT_SKIP).unwrap();

        assert!(dst.join("crap.toml").exists());
        assert!(dst.join("collections/posts.lua").exists());
        assert!(!dst.join("data").exists());
        assert!(!dst.join("uploads").exists());
        assert!(!dst.join("types").exists());
    }

    #[test]
    fn copy_dir_recursive_skip_only_top_level() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        fs::create_dir_all(src.join("data")).unwrap();
        fs::create_dir_all(src.join("subdir/data")).unwrap();
        fs::write(src.join("data/file.txt"), "top data").unwrap();
        fs::write(src.join("subdir/data/file.txt"), "nested data").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, &["data"]).unwrap();

        assert!(
            !dst.join("data").exists(),
            "top-level data/ should be skipped"
        );
        assert!(
            dst.join("subdir/data/file.txt").exists(),
            "nested data/ should NOT be skipped"
        );
    }

    #[test]
    fn count_lua_files_basic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("collections");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("posts.lua"), "").unwrap();
        fs::write(dir.join("tags.lua"), "").unwrap();
        fs::write(dir.join("readme.md"), "").unwrap();

        assert_eq!(count_lua_files(&dir), 2);
        assert_eq!(count_lua_files(&tmp.path().join("nope")), 0);
    }

    #[test]
    fn count_lua_files_mixed_extensions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("test");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.lua"), "").unwrap();
        fs::write(dir.join("b.lua"), "").unwrap();
        fs::write(dir.join("c.txt"), "").unwrap();
        fs::write(dir.join("d.rs"), "").unwrap();
        fs::write(dir.join("e"), "").unwrap();

        assert_eq!(count_lua_files(&dir), 2);
    }

    #[test]
    fn count_lua_files_empty_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("empty");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(count_lua_files(&dir), 0);
    }
}
