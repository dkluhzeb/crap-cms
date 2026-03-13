//! Blueprint management — save, use, list, remove reusable config directory templates.

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Manifest filename written to each saved blueprint.
const MANIFEST_FILENAME: &str = ".crap-blueprint.toml";

/// Metadata about a saved blueprint.
#[derive(Debug, Serialize, Deserialize)]
struct BlueprintManifest {
    /// CMS version that created this blueprint.
    crap_version: String,
    /// ISO 8601 timestamp when the blueprint was saved.
    created_at: Option<String>,
}

impl BlueprintManifest {
    /// Create a new manifest for the current CMS version.
    fn new() -> Self {
        Self {
            crap_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: Some(Utc::now().to_rfc3339()),
        }
    }
}

/// Write a blueprint manifest file to the given directory.
fn write_manifest(dir: &Path) -> Result<()> {
    let manifest = BlueprintManifest::new();
    let content =
        toml::to_string_pretty(&manifest).context("Failed to serialize blueprint manifest")?;
    fs::write(dir.join(MANIFEST_FILENAME), content)
        .with_context(|| format!("Failed to write manifest to '{}'", dir.display()))?;
    Ok(())
}

/// Read a blueprint manifest from the given directory. Returns `None` if the
/// manifest file does not exist (backward compatible with old blueprints).
fn read_manifest(dir: &Path) -> Result<Option<BlueprintManifest>> {
    let path = dir.join(MANIFEST_FILENAME);

    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read manifest from '{}'", path.display()))?;
    let manifest: BlueprintManifest = toml::from_str(&content)
        .with_context(|| format!("Failed to parse manifest from '{}'", path.display()))?;
    Ok(Some(manifest))
}

/// Check the blueprint version against the running binary version.
///
/// Returns `None` if compatible, `Some(message)` on mismatch.
/// Uses the same prefix-match logic as `CrapConfig::check_version()`.
fn check_blueprint_version(blueprint_version: &str) -> Option<String> {
    check_blueprint_version_against(blueprint_version, env!("CARGO_PKG_VERSION"))
}

/// Inner version check, takes explicit values for testability.
fn check_blueprint_version_against(blueprint_version: &str, pkg_version: &str) -> Option<String> {
    // Exact match
    if blueprint_version == pkg_version {
        return None;
    }

    // Prefix match: "0.1" matches "0.1.0", "0.1.3", etc.
    if pkg_version.starts_with(blueprint_version)
        && pkg_version.as_bytes().get(blueprint_version.len()) == Some(&b'.')
    {
        return None;
    }

    Some(format!(
        "Blueprint was created with crap-cms v{}, but running version is v{}",
        blueprint_version, pkg_version
    ))
}

/// Resolve the global blueprints directory.
///
/// - Linux: `~/.config/crap-cms/blueprints/`
/// - macOS: `~/Library/Application Support/crap-cms/blueprints/`
/// - Windows: `C:\Users\<user>\AppData\Roaming\crap-cms\blueprints\`
fn blueprints_dir() -> Result<PathBuf> {
    let base = dirs::config_dir()
        .ok_or_else(|| anyhow!("Could not determine config directory for your platform"))?;
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
        bail!(
            "Directory '{}' does not contain a crap.toml — not a valid config directory",
            config_dir.display()
        );
    }

    let bp_dir = blueprints_dir()?;
    let target = bp_dir.join(name);

    if target.exists() && !force {
        bail!(
            "Blueprint '{}' already exists — use --force to overwrite",
            name
        );
    }

    // Clean target if overwriting
    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("Failed to remove existing blueprint '{}'", name))?;
    }

    fs::create_dir_all(&target).with_context(|| {
        format!(
            "Failed to create blueprint directory '{}'",
            target.display()
        )
    })?;

    copy_dir_recursive(config_dir, &target, BLUEPRINT_SKIP)
        .with_context(|| format!("Failed to copy config to blueprint '{}'", name))?;

    // Write version manifest
    write_manifest(&target)
        .with_context(|| format!("Failed to write manifest for blueprint '{}'", name))?;

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
            bail!(
                "Blueprint '{}' not found. No blueprints saved yet.\nSave one with: crap-cms blueprint save <dir> <name>",
                name
            );
        } else {
            bail!(
                "Blueprint '{}' not found. Available blueprints: {}",
                name,
                available.join(", ")
            );
        }
    }

    // Check blueprint version compatibility (warn but don't block)
    if let Some(manifest) = read_manifest(&source)?
        && let Some(warning) = check_blueprint_version(&manifest.crap_version)
    {
        eprintln!("Warning: {}", warning);
    }

    let target = dir.unwrap_or_else(|| PathBuf::from("./crap-cms"));

    // Refuse to overwrite existing config
    if target.join("crap.toml").exists() {
        bail!(
            "Directory '{}' already contains a crap.toml — refusing to overwrite",
            target.display()
        );
    }

    fs::create_dir_all(&target)
        .with_context(|| format!("Failed to create directory '{}'", target.display()))?;

    copy_dir_recursive(&source, &target, &[]).with_context(|| {
        format!(
            "Failed to copy blueprint '{}' to '{}'",
            name,
            target.display()
        )
    })?;

    // Write types/crap.lua — blueprints skip the types/ dir during save,
    // so we regenerate it from the compiled-in source.
    let types_dir = target.join("types");
    fs::create_dir_all(&types_dir).context("Failed to create types/")?;
    fs::write(types_dir.join("crap.lua"), super::init::LUA_API_TYPES)
        .context("Failed to write types/crap.lua")?;

    let abs = target.canonicalize().unwrap_or_else(|_| target.clone());
    println!(
        "Created project from blueprint '{}': {}",
        name,
        abs.display()
    );
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
        // Show version from manifest if available
        let version_tag = match read_manifest(&bp_path) {
            Ok(Some(m)) => format!(" [v{}]", m.crap_version),
            _ => String::new(),
        };
        println!(
            "  {} ({} collection(s), {} global(s)){}",
            name, collections, globals, version_tag
        );
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
        bail!("Blueprint '{}' not found", name);
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
fn validate_blueprint_name(name: &str) -> Result<()> {
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

#[cfg(test)]
mod tests;
