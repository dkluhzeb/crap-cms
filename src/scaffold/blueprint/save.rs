//! Save a config directory as a named blueprint.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};

use crate::cli;

use super::helpers::{BLUEPRINT_SKIP, blueprints_dir, copy_dir_recursive, validate_blueprint_name};
use super::manifest::write_manifest;

/// Save a config directory as a named blueprint.
///
/// Copies everything except runtime artifacts (`data/`, `uploads/`, `types/`)
/// to `~/.config/crap-cms/blueprints/<name>/`.
pub fn blueprint_save(config_dir: &Path, name: &str, force: bool) -> Result<()> {
    validate_blueprint_name(name)?;

    if !config_dir.join("crap.toml").exists() {
        bail!(
            "Directory '{}' does not contain a crap.toml — not a valid config directory",
            config_dir.display()
        );
    }

    let target = blueprints_dir()?.join(name);

    if target.exists() && !force {
        bail!(
            "Blueprint '{}' already exists — use --force to overwrite",
            name
        );
    }

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

    write_manifest(&target)
        .with_context(|| format!("Failed to write manifest for blueprint '{}'", name))?;

    cli::success(&format!(
        "Saved blueprint '{}' from {}",
        name,
        config_dir.display()
    ));
    cli::kv("Location", &target.display().to_string());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scaffold::blueprint::helpers::{BLUEPRINT_SKIP, with_temp_config_dir};
    use crate::scaffold::blueprint::manifest::{MANIFEST_FILENAME, read_manifest};

    #[test]
    fn save_requires_crap_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = blueprint_save(tmp.path(), "test", false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("crap.toml"));
    }

    #[test]
    fn save_already_exists_no_force() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");
            fs::write(tmp.path().join("crap.toml"), "").unwrap();

            let bp_dir = config_home.join("crap-cms").join("blueprints");
            let bp_target = bp_dir.join("my-bp");
            fs::create_dir_all(&bp_target).unwrap();

            let result = blueprint_save(tmp.path(), "my-bp", false);
            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("already exists"), "got: {}", err);
            assert!(err.contains("--force"), "got: {}", err);
        });
    }

    #[test]
    fn save_force_overwrites() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");
            fs::write(
                tmp.path().join("crap.toml"),
                "[server]\nadmin_port = 3000\n",
            )
            .unwrap();
            fs::write(tmp.path().join("init.lua"), "-- hello").unwrap();

            let bp_dir = config_home.join("crap-cms").join("blueprints");
            let bp_target = bp_dir.join("overwrite-bp");
            fs::create_dir_all(&bp_target).unwrap();
            fs::write(bp_target.join("old-file.txt"), "old content").unwrap();

            let result = blueprint_save(tmp.path(), "overwrite-bp", true);
            assert!(
                result.is_ok(),
                "blueprint_save with force failed: {:?}",
                result
            );

            assert!(!bp_target.join("old-file.txt").exists());
            assert!(bp_target.join("crap.toml").exists());
            assert!(bp_target.join(MANIFEST_FILENAME).exists());
        });
    }

    #[test]
    fn save_success() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");

            fs::create_dir_all(tmp.path().join("collections")).unwrap();
            fs::create_dir_all(tmp.path().join("data")).unwrap();
            fs::create_dir_all(tmp.path().join("uploads")).unwrap();
            fs::create_dir_all(tmp.path().join("types")).unwrap();
            fs::write(
                tmp.path().join("crap.toml"),
                "[server]\nadmin_port = 3000\n",
            )
            .unwrap();
            fs::write(tmp.path().join("collections/posts.lua"), "-- posts").unwrap();
            fs::write(tmp.path().join("data/crap.db"), "should skip").unwrap();
            fs::write(tmp.path().join("uploads/photo.jpg"), "should skip").unwrap();
            fs::write(tmp.path().join("types/crap.lua"), "should skip").unwrap();

            let result = blueprint_save(tmp.path(), "new-bp", false);
            assert!(result.is_ok(), "blueprint_save failed: {:?}", result);

            let bp_target = config_home
                .join("crap-cms")
                .join("blueprints")
                .join("new-bp");
            assert!(bp_target.join("crap.toml").exists());
            assert!(bp_target.join("collections/posts.lua").exists());
            assert!(bp_target.join(MANIFEST_FILENAME).exists());
            assert!(!bp_target.join("data").exists());
            assert!(!bp_target.join("uploads").exists());
            assert!(!bp_target.join("types").exists());
        });
    }

    #[test]
    fn roundtrip_save_and_copy() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let config = tmp.path().join("my-config");
        fs::create_dir_all(config.join("collections")).unwrap();
        fs::create_dir_all(config.join("data")).unwrap();
        fs::create_dir_all(config.join("uploads")).unwrap();
        fs::write(config.join("crap.toml"), "[server]\nadmin_port = 4000\n").unwrap();
        fs::write(config.join("init.lua"), "-- hello").unwrap();
        fs::write(config.join("collections/posts.lua"), "-- posts").unwrap();
        fs::write(config.join("data/crap.db"), "should be skipped").unwrap();
        fs::write(config.join("uploads/photo.jpg"), "should be skipped").unwrap();

        let bp_target = tmp.path().join("blueprints").join("my-blog");
        fs::create_dir_all(&bp_target).unwrap();

        copy_dir_recursive(&config, &bp_target, BLUEPRINT_SKIP).unwrap();
        write_manifest(&bp_target).unwrap();

        assert!(bp_target.join("crap.toml").exists());
        assert!(bp_target.join("init.lua").exists());
        assert!(bp_target.join("collections/posts.lua").exists());
        assert!(!bp_target.join("data").exists());
        assert!(!bp_target.join("uploads").exists());

        assert!(bp_target.join(MANIFEST_FILENAME).exists());
        let manifest = read_manifest(&bp_target)
            .unwrap()
            .expect("manifest should exist");
        assert_eq!(manifest.crap_version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.created_at.is_some());

        // "Use" the blueprint
        let new_project = tmp.path().join("new-project");
        fs::create_dir_all(&new_project).unwrap();
        copy_dir_recursive(&bp_target, &new_project, &[]).unwrap();

        assert!(new_project.join("crap.toml").exists());
        assert!(new_project.join("init.lua").exists());
        assert!(new_project.join("collections/posts.lua").exists());
        assert!(new_project.join(MANIFEST_FILENAME).exists());

        let toml = fs::read_to_string(new_project.join("crap.toml")).unwrap();
        assert!(toml.contains("admin_port = 4000"));
    }
}
