//! Blueprint management — save, use, list, remove reusable config directory templates.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve the global blueprints directory.
///
/// - Linux: `~/.config/crap-cms/blueprints/`
/// - macOS: `~/Library/Application Support/crap-cms/blueprints/`
/// - Windows: `C:\Users\<user>\AppData\Roaming\crap-cms\blueprints\`
fn blueprints_dir() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine config directory for your platform"))?;
    Ok(base.join("crap-cms").join("blueprints"))
}

/// Files and directories to skip when saving a blueprint (runtime artifacts).
const BLUEPRINT_SKIP: &[&str] = &["data", "uploads", "types"];

/// Save a config directory as a named blueprint.
///
/// Copies everything except runtime artifacts (`data/`, `uploads/`, `types/`)
/// to `~/.config/crap-cms/blueprints/<name>/`.
pub fn blueprint_save(config_dir: &Path, name: &str, force: bool) -> Result<()> {
    validate_blueprint_name(name)?;

    // Verify it's actually a config directory
    if !config_dir.join("crap.toml").exists() {
        anyhow::bail!(
            "Directory '{}' does not contain a crap.toml — not a valid config directory",
            config_dir.display()
        );
    }

    let bp_dir = blueprints_dir()?;
    let target = bp_dir.join(name);

    if target.exists() && !force {
        anyhow::bail!(
            "Blueprint '{}' already exists — use --force to overwrite",
            name
        );
    }

    // Clean target if overwriting
    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("Failed to remove existing blueprint '{}'", name))?;
    }

    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create blueprint directory '{}'", target.display()))?;

    copy_dir_recursive(config_dir, &target, BLUEPRINT_SKIP)
        .with_context(|| format!("Failed to copy config to blueprint '{}'", name))?;

    println!("Saved blueprint '{}' from {}", name, config_dir.display());
    println!("  Location: {}", target.display());

    Ok(())
}

/// Create a new project from a saved blueprint.
///
/// Copies the blueprint to `dir` (or `./crap-cms/` if omitted).
pub fn blueprint_use(name: &str, dir: Option<PathBuf>) -> Result<()> {
    validate_blueprint_name(name)?;

    let bp_dir = blueprints_dir()?;
    let source = bp_dir.join(name);

    if !source.exists() {
        let available = list_blueprint_names()?;
        if available.is_empty() {
            anyhow::bail!("Blueprint '{}' not found. No blueprints saved yet.\nSave one with: crap-cms blueprint save <dir> <name>", name);
        } else {
            anyhow::bail!(
                "Blueprint '{}' not found. Available blueprints: {}",
                name,
                available.join(", ")
            );
        }
    }

    let target = dir.unwrap_or_else(|| PathBuf::from("./crap-cms"));

    // Refuse to overwrite existing config
    if target.join("crap.toml").exists() {
        anyhow::bail!(
            "Directory '{}' already contains a crap.toml — refusing to overwrite",
            target.display()
        );
    }

    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create directory '{}'", target.display()))?;

    copy_dir_recursive(&source, &target, &[])
        .with_context(|| format!("Failed to copy blueprint '{}' to '{}'", name, target.display()))?;

    let abs = target.canonicalize().unwrap_or_else(|_| target.clone());
    println!("Created project from blueprint '{}': {}", name, abs.display());
    println!();
    println!("Start the server: crap-cms serve {}", target.display());

    Ok(())
}

/// List all saved blueprints.
pub fn blueprint_list() -> Result<()> {
    let bp_dir = blueprints_dir()?;

    if !bp_dir.exists() {
        println!("No blueprints saved yet.");
        println!("Save one with: crap-cms blueprint save <dir> <name>");
        return Ok(());
    }

    let names = list_blueprint_names()?;
    if names.is_empty() {
        println!("No blueprints saved yet.");
        println!("Save one with: crap-cms blueprint save <dir> <name>");
        return Ok(());
    }

    println!("Saved blueprints:");
    for name in &names {
        let bp_path = bp_dir.join(name);
        // Count collections and globals for a quick summary
        let collections = count_lua_files(&bp_path.join("collections"));
        let globals = count_lua_files(&bp_path.join("globals"));
        println!("  {} ({} collection(s), {} global(s))", name, collections, globals);
    }
    println!();
    println!("Use with: crap-cms blueprint use <name> [dir]");

    Ok(())
}

/// Remove a saved blueprint.
pub fn blueprint_remove(name: &str) -> Result<()> {
    validate_blueprint_name(name)?;

    let bp_dir = blueprints_dir()?;
    let target = bp_dir.join(name);

    if !target.exists() {
        anyhow::bail!("Blueprint '{}' not found", name);
    }

    fs::remove_dir_all(&target)
        .with_context(|| format!("Failed to remove blueprint '{}'", name))?;

    println!("Removed blueprint '{}'", name);

    Ok(())
}

/// List blueprint names from the global blueprints directory.
pub fn list_blueprint_names() -> Result<Vec<String>> {
    let bp_dir = blueprints_dir()?;
    if !bp_dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(&bp_dir)? {
        let entry = entry?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Recursively copy a directory, skipping entries whose names match `skip`.
fn copy_dir_recursive(src: &Path, dst: &Path, skip: &[&str]) -> Result<()> {
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
fn count_lua_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path().extension().map(|ext| ext == "lua").unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Validate a blueprint name: alphanumeric, hyphens, underscores.
fn validate_blueprint_name(name: &str) -> Result<()> {
    if name.is_empty() {
        anyhow::bail!("Blueprint name cannot be empty");
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!(
            "Invalid blueprint name '{}' — use alphanumeric characters, hyphens, and underscores only",
            name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_validate_blueprint_name() {
        assert!(validate_blueprint_name("blog").is_ok());
        assert!(validate_blueprint_name("my-blog").is_ok());
        assert!(validate_blueprint_name("blog_v2").is_ok());
        assert!(validate_blueprint_name("").is_err());
        assert!(validate_blueprint_name("bad name").is_err());
        assert!(validate_blueprint_name("bad/name").is_err());
    }

    #[test]
    fn test_copy_dir_recursive() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        // Build a small tree
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
    fn test_blueprint_save_requires_crap_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Empty dir — no crap.toml
        let result = blueprint_save(tmp.path(), "test", false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("crap.toml"));
    }

    #[test]
    fn test_blueprint_use_not_found() {
        let result = blueprint_use("nonexistent_test_bp_12345", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_blueprint_remove_not_found() {
        let result = blueprint_remove("nonexistent_test_bp_12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_count_lua_files() {
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
    fn test_copy_dir_recursive_nested_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        // Build a deeper nested tree
        fs::create_dir_all(src.join("a/b/c")).unwrap();
        fs::write(src.join("a/b/c/deep.txt"), "deep content").unwrap();
        fs::write(src.join("a/top.txt"), "top content").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, &[]).unwrap();

        assert!(dst.join("a/b/c/deep.txt").exists());
        assert_eq!(
            fs::read_to_string(dst.join("a/b/c/deep.txt")).unwrap(),
            "deep content"
        );
        assert!(dst.join("a/top.txt").exists());
        assert_eq!(
            fs::read_to_string(dst.join("a/top.txt")).unwrap(),
            "top content"
        );
    }

    #[test]
    fn test_copy_dir_recursive_skip_multiple() {
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
        assert!(!dst.join("data").exists(), "data/ should be skipped");
        assert!(!dst.join("uploads").exists(), "uploads/ should be skipped");
        assert!(!dst.join("types").exists(), "types/ should be skipped");
    }

    #[test]
    fn test_copy_dir_recursive_skip_only_top_level() {
        // Skip should only apply at the top level of copy_dir_recursive
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");

        // "data" at top level should be skipped, but nested "data" should not
        fs::create_dir_all(src.join("data")).unwrap();
        fs::create_dir_all(src.join("subdir/data")).unwrap();
        fs::write(src.join("data/file.txt"), "top data").unwrap();
        fs::write(src.join("subdir/data/file.txt"), "nested data").unwrap();

        fs::create_dir_all(&dst).unwrap();
        copy_dir_recursive(&src, &dst, &["data"]).unwrap();

        assert!(!dst.join("data").exists(), "top-level data/ should be skipped");
        // Nested "data" inside "subdir" should NOT be skipped because skip
        // only applies at the top level; the recursive call passes &[].
        assert!(dst.join("subdir/data/file.txt").exists(), "nested data/ should NOT be skipped");
    }

    #[test]
    fn test_count_lua_files_mixed_extensions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("test");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.lua"), "").unwrap();
        fs::write(dir.join("b.lua"), "").unwrap();
        fs::write(dir.join("c.txt"), "").unwrap();
        fs::write(dir.join("d.rs"), "").unwrap();
        fs::write(dir.join("e"), "").unwrap(); // no extension
        assert_eq!(count_lua_files(&dir), 2);
    }

    #[test]
    fn test_count_lua_files_empty_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("empty");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(count_lua_files(&dir), 0);
    }

    #[test]
    fn test_validate_blueprint_name_special_chars() {
        assert!(validate_blueprint_name("abc123").is_ok());
        assert!(validate_blueprint_name("my_blog_v2").is_ok());
        assert!(validate_blueprint_name("A-B-C").is_ok());
        assert!(validate_blueprint_name("a.b").is_err());
        assert!(validate_blueprint_name("a/b").is_err());
        assert!(validate_blueprint_name("a\\b").is_err());
        assert!(validate_blueprint_name("a b").is_err());
        assert!(validate_blueprint_name("a@b").is_err());
    }

    #[test]
    fn test_blueprint_roundtrip() {
        // Save a blueprint and use it to create a new project
        let tmp = tempfile::tempdir().expect("tempdir");

        // Create a fake config dir
        let config = tmp.path().join("my-config");
        fs::create_dir_all(config.join("collections")).unwrap();
        fs::create_dir_all(config.join("data")).unwrap();
        fs::create_dir_all(config.join("uploads")).unwrap();
        fs::write(config.join("crap.toml"), "[server]\nadmin_port = 4000\n").unwrap();
        fs::write(config.join("init.lua"), "-- hello").unwrap();
        fs::write(config.join("collections/posts.lua"), "-- posts").unwrap();
        fs::write(config.join("data/crap.db"), "should be skipped").unwrap();
        fs::write(config.join("uploads/photo.jpg"), "should be skipped").unwrap();

        // Save as blueprint (use a custom dir to avoid polluting real config)
        // We test the internal helpers instead of the full save/use flow
        // since those depend on the global config dir
        let bp_dir = tmp.path().join("blueprints");
        fs::create_dir_all(&bp_dir).unwrap();
        let bp_target = bp_dir.join("my-blog");
        fs::create_dir_all(&bp_target).unwrap();

        copy_dir_recursive(&config, &bp_target, BLUEPRINT_SKIP).unwrap();

        // Verify blueprint contents
        assert!(bp_target.join("crap.toml").exists());
        assert!(bp_target.join("init.lua").exists());
        assert!(bp_target.join("collections/posts.lua").exists());
        assert!(!bp_target.join("data").exists(), "data/ should be excluded");
        assert!(!bp_target.join("uploads").exists(), "uploads/ should be excluded");

        // "Use" the blueprint to create a new project
        let new_project = tmp.path().join("new-project");
        fs::create_dir_all(&new_project).unwrap();
        copy_dir_recursive(&bp_target, &new_project, &[]).unwrap();

        assert!(new_project.join("crap.toml").exists());
        assert!(new_project.join("init.lua").exists());
        assert!(new_project.join("collections/posts.lua").exists());

        let toml = fs::read_to_string(new_project.join("crap.toml")).unwrap();
        assert!(toml.contains("admin_port = 4000"));
    }
}
