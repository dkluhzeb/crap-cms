//! Save a config directory as a named blueprint.
//!
//! The copy is built in a hidden staging directory beside the blueprints
//! (`.<name>.saving-<id>`) and swapped into place only once it is complete,
//! so a failed save — including `--force` over an existing blueprint — leaves
//! the previous blueprint untouched. What is left out is decided in
//! [`super::exclude`].

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use nanoid::nanoid;

use crate::{cli, config::CrapConfig};

use super::{
    exclude::SaveExclusions,
    helpers::{blueprints_dir, copy_dir_recursive, report_skipped_links, validate_blueprint_name},
    manifest::write_manifest,
};

/// A blueprint being written under a hidden name next to its final one.
/// Removed on drop unless it was published.
struct StagedBlueprint {
    path: PathBuf,
    published: bool,
}

impl StagedBlueprint {
    /// Create the staging directory for blueprint `name` inside `bp_dir`.
    fn new(bp_dir: &Path, name: &str) -> Result<Self> {
        let path = bp_dir.join(format!(".{name}.saving-{}", nanoid!(10)));

        fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create '{}'", path.display()))?;

        Ok(Self {
            path,
            published: false,
        })
    }

    /// Move the complete copy to `target`. An existing blueprint there is
    /// renamed aside first and restored when the swap fails, then removed.
    fn publish(mut self, target: &Path) -> Result<()> {
        let aside = target.exists().then(|| aside_path(target));

        if let Some(aside) = &aside {
            fs::rename(target, aside)
                .with_context(|| format!("Failed to move '{}' aside", target.display()))?;
        }

        if let Err(e) = fs::rename(&self.path, target) {
            if let Some(aside) = &aside {
                let _ = fs::rename(aside, target);
            }

            return Err(e).with_context(|| format!("Failed to publish '{}'", target.display()));
        }

        self.published = true;

        if let Some(aside) = aside {
            let _ = fs::remove_dir_all(aside);
        }

        Ok(())
    }
}

impl Drop for StagedBlueprint {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Hidden name the replaced blueprint is kept under during the swap.
fn aside_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    target.with_file_name(format!(".{name}.old-{}", nanoid!(10)))
}

/// Copy the project at `root` into `dest` (minus the exclusions) and write
/// the blueprint manifest.
///
/// The config is only parsed (for the database and log paths it names), not
/// loaded: a load resolves the auth secret and would generate and persist
/// one into the project being saved when it has none yet.
fn copy_project(root: &Path, dest: &Path) -> Result<()> {
    let cfg = CrapConfig::parse(root)?;
    let exclusions = SaveExclusions::new(root, &cfg);

    let skipped_links = copy_dir_recursive(root, dest, &|path: &Path| exclusions.excludes(path))?;
    report_skipped_links(&skipped_links);

    write_manifest(dest)
}

/// Save a config directory as a named blueprint.
///
/// Copies the project's definitions to `~/.config/crap-cms/blueprints/<name>/`,
/// leaving out its state and secrets (see [`super::exclude`]).
///
/// # Errors
///
/// Returns an error if the name is invalid, `config_dir` lacks a `crap.toml`
/// or its config does not load, the destination already exists without
/// `--force`, or copying fails.
pub fn blueprint_save(config_dir: &Path, name: &str, force: bool) -> Result<()> {
    validate_blueprint_name(name)?;

    if !config_dir.join("crap.toml").exists() {
        bail!(
            "Directory '{}' does not contain a crap.toml -- not a valid config directory",
            config_dir.display()
        );
    }

    let root = config_dir
        .canonicalize()
        .with_context(|| format!("Failed to resolve '{}'", config_dir.display()))?;
    let bp_dir = blueprints_dir()?;
    let target = bp_dir.join(name);

    if target.exists() && !force {
        bail!("Blueprint '{name}' already exists -- use --force to overwrite");
    }

    let staged = StagedBlueprint::new(&bp_dir, name)?;

    copy_project(&root, &staged.path)
        .with_context(|| format!("Failed to copy config to blueprint '{name}'"))?;

    staged.publish(&target)?;

    cli::success(&format!("Saved blueprint '{name}' from {}", root.display()));
    cli::kv("Location", &target.display().to_string());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use crate::scaffold::blueprint::helpers::{keep_all, with_temp_config_dir};
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
            assert!(err.contains("already exists"), "got: {err}");
            assert!(err.contains("--force"), "got: {err}");
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
                "blueprint_save with force failed: {result:?}"
            );

            assert!(!bp_target.join("old-file.txt").exists());
            assert!(bp_target.join("crap.toml").exists());
            assert!(bp_target.join(MANIFEST_FILENAME).exists());
        });
    }

    /// Regression: saving loaded the project's config, which resolves the
    /// auth secret — a project that never ran got a freshly generated
    /// `data/.jwt_secret` written into it by `blueprint save`.
    #[test]
    fn save_does_not_write_into_the_project() {
        with_temp_config_dir(|_| {
            let tmp = tempfile::tempdir().expect("tempdir");
            fs::write(tmp.path().join("crap.toml"), "").unwrap();

            blueprint_save(tmp.path(), "pristine", false).unwrap();

            let entries: Vec<OsString> = fs::read_dir(tmp.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(entries, vec![OsString::from("crap.toml")]);
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
            assert!(result.is_ok(), "blueprint_save failed: {result:?}");

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
    fn roundtrip_save_and_use() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");

            let config = tmp.path().join("my-config");
            fs::create_dir_all(config.join("collections")).unwrap();
            fs::create_dir_all(config.join("uploads")).unwrap();
            fs::write(config.join("crap.toml"), "[server]\nadmin_port = 4000\n").unwrap();
            fs::write(config.join("init.lua"), "-- hello").unwrap();
            fs::write(config.join("collections/posts.lua"), "-- posts").unwrap();
            fs::write(config.join("uploads/photo.jpg"), "should be skipped").unwrap();

            blueprint_save(&config, "my-blog", false).unwrap();

            let bp_target = config_home.join("crap-cms/blueprints/my-blog");
            assert!(bp_target.join("crap.toml").exists());
            assert!(bp_target.join("init.lua").exists());
            assert!(bp_target.join("collections/posts.lua").exists());
            assert!(!bp_target.join("data").exists());
            assert!(!bp_target.join("uploads").exists());

            let manifest = read_manifest(&bp_target)
                .unwrap()
                .expect("manifest should exist");
            assert_eq!(manifest.crap_version, env!("CARGO_PKG_VERSION"));
            assert!(manifest.created_at.is_some());

            // "Use" the blueprint
            let new_project = tmp.path().join("new-project");
            fs::create_dir_all(&new_project).unwrap();
            copy_dir_recursive(&bp_target, &new_project, &keep_all).unwrap();

            let toml = fs::read_to_string(new_project.join("crap.toml")).unwrap();
            assert!(toml.contains("admin_port = 4000"));
            assert!(new_project.join(MANIFEST_FILENAME).exists());
        });
    }

    /// Regression: `backups/` (database snapshots and the auth secret that
    /// unseals them) and a database configured outside `data/` were copied
    /// into the reusable blueprint.
    #[test]
    fn save_leaves_out_backups_and_a_database_outside_data() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let project = tmp.path();
            fs::write(
                project.join("crap.toml"),
                "[database]\npath = \"site.db\"\n",
            )
            .unwrap();
            fs::write(project.join("site.db"), "db").unwrap();
            fs::write(project.join("site.db-wal"), "wal").unwrap();
            let backup = project.join("backups/backup-1");
            fs::create_dir_all(&backup).unwrap();
            fs::write(backup.join("crap.db"), "db").unwrap();
            fs::write(backup.join("jwt_secret"), "secret").unwrap();
            fs::write(backup.join("manifest.json"), "{}").unwrap();

            blueprint_save(project, "clean", false).unwrap();

            let bp = config_home.join("crap-cms/blueprints/clean");
            assert!(bp.join("crap.toml").exists());
            assert!(!bp.join("backups").exists());
            assert!(!bp.join("site.db").exists());
            assert!(!bp.join("site.db-wal").exists());
            assert!(!bp.join("data").exists());
        });
    }

    /// Regression: `--force` removed the old blueprint before copying, so a
    /// failed copy lost it.
    #[cfg(unix)]
    #[test]
    fn a_failed_forced_save_keeps_the_previous_blueprint() {
        with_temp_config_dir(|config_home| {
            let tmp = tempfile::tempdir().expect("tempdir");
            let project = tmp.path();
            fs::write(project.join("crap.toml"), "").unwrap();
            let unreadable = project.join("unreadable.lua");
            fs::write(&unreadable, "-- x").unwrap();
            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

            // Root reads anything; the failure can't be provoked then.
            if fs::read(&unreadable).is_ok() {
                return;
            }

            let bp_dir = config_home.join("crap-cms/blueprints");
            fs::create_dir_all(bp_dir.join("keep")).unwrap();
            fs::write(bp_dir.join("keep/old.lua"), "old").unwrap();

            assert!(blueprint_save(project, "keep", true).is_err());

            assert_eq!(
                fs::read_to_string(bp_dir.join("keep/old.lua")).unwrap(),
                "old"
            );
            let leftovers: Vec<_> = fs::read_dir(&bp_dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(leftovers, vec![OsString::from("keep")]);

            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
        });
    }
}
