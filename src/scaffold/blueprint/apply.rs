//! Create a new project from a saved blueprint.

use std::{fs, path::PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::cli;

use super::helpers::{blueprints_dir, copy_dir_recursive, validate_blueprint_name};
use super::list::list_blueprint_names;
use super::manifest::{check_blueprint_version, read_manifest};

/// Create a new project from a saved blueprint.
///
/// Copies the blueprint to `dir` (or `./crap-cms/` if omitted).
pub fn blueprint_use(name: &str, dir: Option<PathBuf>) -> Result<()> {
    validate_blueprint_name(name)?;

    let source = blueprints_dir()?.join(name);

    if !source.exists() {
        return Err(not_found_error(name)?);
    }

    if let Some(manifest) = read_manifest(&source)?
        && let Some(warning) = check_blueprint_version(&manifest.crap_version)
    {
        cli::warning(&warning);
    }

    let target = dir.unwrap_or_else(|| PathBuf::from("./crap-cms"));

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

    // Regenerate types/crap.lua — blueprints skip types/ during save.
    let types_dir = target.join("types");
    fs::create_dir_all(&types_dir).context("Failed to create types/")?;
    fs::write(
        types_dir.join("crap.lua"),
        super::super::init::LUA_API_TYPES,
    )
    .context("Failed to write types/crap.lua")?;

    let abs = target.canonicalize().unwrap_or_else(|_| target.clone());
    cli::success(&format!(
        "Created project from blueprint '{}': {}",
        name,
        abs.display()
    ));
    cli::hint(&format!(
        "Start the server: crap-cms serve {}",
        target.display()
    ));

    Ok(())
}

/// Build an informative "not found" error listing available blueprints.
fn not_found_error(name: &str) -> Result<anyhow::Error> {
    let available = list_blueprint_names()?;

    if available.is_empty() {
        bail!(
            "Blueprint '{}' not found. No blueprints saved yet.\nSave one with: crap-cms blueprint save <dir> <name>",
            name
        );
    }

    bail!(
        "Blueprint '{}' not found. Available blueprints: {}",
        name,
        available.join(", ")
    );
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::scaffold::blueprint::helpers::with_temp_config_dir;
    use crate::scaffold::blueprint::manifest::{
        BlueprintManifest, MANIFEST_FILENAME, write_manifest,
    };
    use crate::scaffold::init::LUA_API_TYPES;

    #[test]
    fn use_not_found() {
        let result = blueprint_use("nonexistent_test_bp_12345", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn use_not_found_with_others_available() {
        with_temp_config_dir(|config_home| {
            let bp_dir = config_home.join("crap-cms").join("blueprints");
            fs::create_dir_all(bp_dir.join("other-bp")).unwrap();

            let result = blueprint_use("missing-bp", None);
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("not found"), "got: {}", err);
            assert!(
                err.contains("other-bp"),
                "should list available, got: {}",
                err
            );
        });
    }

    #[test]
    fn use_overwrite_protection() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");

            let bp_dir = config_home.join("crap-cms").join("blueprints");
            let bp_source = bp_dir.join("existing-bp");
            fs::create_dir_all(&bp_source).unwrap();
            fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();

            let target = tmp.path().join("my-project");
            fs::create_dir_all(&target).unwrap();
            fs::write(target.join("crap.toml"), "already here").unwrap();

            let result = blueprint_use("existing-bp", Some(target));
            assert!(result.is_err());
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("already contains a crap.toml")
            );
        });
    }

    #[test]
    fn use_version_mismatch_warning() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");

            let bp_dir = config_home.join("crap-cms").join("blueprints");
            let bp_source = bp_dir.join("old-version-bp");
            fs::create_dir_all(&bp_source).unwrap();
            fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();

            let old_manifest = BlueprintManifest {
                crap_version: "0.0.1-old".to_string(),
                created_at: None,
            };
            let content = toml::to_string_pretty(&old_manifest).unwrap();
            fs::write(bp_source.join(MANIFEST_FILENAME), content).unwrap();

            let target = tmp.path().join("new-project");
            let result = blueprint_use("old-version-bp", Some(target.clone()));
            assert!(result.is_ok(), "should succeed with mismatch: {:?}", result);
            assert!(target.join("crap.toml").exists());
        });
    }

    #[test]
    fn use_success() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");

            let bp_dir = config_home.join("crap-cms").join("blueprints");
            let bp_source = bp_dir.join("good-bp");
            fs::create_dir_all(bp_source.join("collections")).unwrap();
            fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();
            fs::write(bp_source.join("collections/posts.lua"), "-- posts").unwrap();
            write_manifest(&bp_source).unwrap();

            let target = tmp.path().join("my-new-project");
            let result = blueprint_use("good-bp", Some(target.clone()));
            assert!(result.is_ok(), "blueprint_use failed: {:?}", result);

            assert!(target.join("crap.toml").exists());
            assert!(target.join("collections/posts.lua").exists());
            assert!(target.join("types/crap.lua").exists());

            let types_content = fs::read_to_string(target.join("types/crap.lua")).unwrap();
            assert!(!types_content.is_empty());
        });
    }

    #[test]
    fn use_writes_types() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let bp_source = tmp.path().join("blueprint");
        fs::create_dir_all(bp_source.join("collections")).unwrap();
        fs::write(bp_source.join("crap.toml"), "[server]\nadmin_port = 3000\n").unwrap();
        fs::write(bp_source.join("collections/posts.lua"), "-- posts").unwrap();

        let target = tmp.path().join("new-project");
        fs::create_dir_all(&target).unwrap();
        copy_dir_recursive(&bp_source, &target, &[]).unwrap();

        let types_dir = target.join("types");
        fs::create_dir_all(&types_dir).unwrap();
        fs::write(types_dir.join("crap.lua"), LUA_API_TYPES).unwrap();

        let types_file = target.join("types/crap.lua");
        assert!(types_file.exists());
        assert!(!fs::read_to_string(&types_file).unwrap().is_empty());
    }
}
