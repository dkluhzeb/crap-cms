//! Version store layout and symlink management.
//!
//! Layout (all under `$XDG_DATA_HOME/crap-cms/`, default `~/.local/share/crap-cms/`):
//!
//! ```text
//! versions/
//!   v0.1.0-alpha.4/crap-cms
//!   v0.1.0-alpha.5/crap-cms
//! current -> versions/v0.1.0-alpha.5/crap-cms
//! ```
//!
//! The shim on `$PATH` (`~/.local/bin/crap-cms`) points at `current`, which is
//! the single atomic swap point for `crap-cms update use <version>`.

use anyhow::{Context, Result, bail};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Abstracts the per-user store so tests can point it at a temp directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// The default store: `$XDG_DATA_HOME/crap-cms/` (or `~/.local/share/crap-cms/`).
    pub fn default_for_user() -> Result<Self> {
        let data_home = xdg_data_home().context("resolving $XDG_DATA_HOME / $HOME")?;
        Ok(Self::at(data_home.join("crap-cms")))
    }

    /// Build a store rooted at an explicit path (tests).
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    pub fn version_path(&self, version: &str) -> PathBuf {
        self.versions_dir().join(version).join(binary_filename())
    }

    /// Path of the `current` symlink.
    pub fn current_link(&self) -> PathBuf {
        self.root.join("current")
    }

    /// List installed versions (directory names under `versions/`).
    pub fn installed(&self) -> Result<Vec<String>> {
        let dir = self.versions_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Return the version name currently pointed at by `current`, if any.
    pub fn active_version(&self) -> Option<String> {
        let link = self.current_link();
        let target = fs::read_link(&link).ok()?;
        // target = ".../versions/<VER>/crap-cms"
        let ver = target.parent()?.file_name()?.to_str()?.to_string();
        Some(ver)
    }

    /// Install a binary from `src_path` as `<store>/versions/<version>/crap-cms`.
    ///
    /// On success the file is executable. Moves rather than copies when the
    /// source is on the same filesystem, so callers can pass a tempfile path.
    pub fn install_binary(&self, version: &str, src_path: &Path) -> Result<PathBuf> {
        let dest_dir = self.versions_dir().join(version);
        fs::create_dir_all(&dest_dir)
            .with_context(|| format!("creating {}", dest_dir.display()))?;
        let dest_path = dest_dir.join(binary_filename());

        // rename across filesystems may fail → fall back to copy + remove.
        if let Err(rename_err) = fs::rename(src_path, &dest_path) {
            fs::copy(src_path, &dest_path).with_context(|| {
                format!(
                    "copying {} to {} (rename failed: {rename_err})",
                    src_path.display(),
                    dest_path.display()
                )
            })?;
            let _ = fs::remove_file(src_path);
        }

        set_executable(&dest_path)?;
        Ok(dest_path)
    }

    /// Flip the `current` symlink to point at `<version>/crap-cms`.
    ///
    /// Uses symlink-then-rename so the swap is atomic: readers of `current`
    /// either see the old target or the new target, never a missing file.
    pub fn switch_to(&self, version: &str) -> Result<()> {
        let target = self.version_path(version);
        if !target.exists() {
            bail!(
                "version {version} is not installed (expected {}). Run `crap-cms update install {version}` first.",
                target.display()
            );
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("creating {}", self.root.display()))?;

        let link = self.current_link();
        let tmp = self.root.join(".current.new");

        let _ = fs::remove_file(&tmp);
        make_symlink(&target, &tmp)
            .with_context(|| format!("creating temp symlink {}", tmp.display()))?;

        // Atomic rename swap. `rename` replaces a symlink in-place on Linux.
        fs::rename(&tmp, &link).with_context(|| {
            format!(
                "atomically swapping {} -> {}",
                link.display(),
                target.display()
            )
        })?;
        Ok(())
    }

    /// Remove a version from the store. Refuses if it is the active one.
    pub fn uninstall(&self, version: &str) -> Result<()> {
        if self.active_version().as_deref() == Some(version) {
            bail!(
                "cannot uninstall the active version {version}: switch to another version first with `crap-cms update use <other>`"
            );
        }
        let dir = self.versions_dir().join(version);
        if !dir.exists() {
            bail!("version {version} is not installed");
        }
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        Ok(())
    }

    /// Does the given path live inside this store's versions tree?
    pub fn owns_path(&self, path: &Path) -> bool {
        let versions_canonical = self.versions_dir().canonicalize().ok();
        let path_canonical = path.canonicalize().ok();
        match (versions_canonical, path_canonical) {
            (Some(root), Some(p)) => p.starts_with(&root),
            _ => false,
        }
    }
}

fn xdg_data_home() -> Option<PathBuf> {
    if let Ok(val) = std::env::var("XDG_DATA_HOME")
        && !val.is_empty()
    {
        return Some(PathBuf::from(val));
    }
    std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|home| PathBuf::from(home).join(".local").join("share"))
}

/// Binary filename inside a version directory (includes `.exe` on Windows).
pub fn binary_filename() -> &'static str {
    if cfg!(windows) {
        "crap-cms.exe"
    } else {
        "crap-cms"
    }
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn fake_binary(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(b"fake binary contents").unwrap();
        p
    }

    #[test]
    fn install_writes_binary_and_is_listed() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());

        let src_dir = TempDir::new().unwrap();
        let src = fake_binary(src_dir.path(), "crap-cms");

        let dest = store.install_binary("v0.1.0-alpha.5", &src).unwrap();
        assert!(dest.exists(), "binary must land under versions/");
        assert!(
            store
                .installed()
                .unwrap()
                .contains(&"v0.1.0-alpha.5".to_string())
        );
    }

    #[test]
    fn switch_to_flips_current_symlink() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());

        let src_dir = TempDir::new().unwrap();
        let src1 = fake_binary(src_dir.path(), "crap-cms1");
        let src2 = fake_binary(src_dir.path(), "crap-cms2");
        store.install_binary("v0.1.0-alpha.4", &src1).unwrap();
        store.install_binary("v0.1.0-alpha.5", &src2).unwrap();

        store.switch_to("v0.1.0-alpha.5").unwrap();
        assert_eq!(store.active_version().as_deref(), Some("v0.1.0-alpha.5"));

        store.switch_to("v0.1.0-alpha.4").unwrap();
        assert_eq!(store.active_version().as_deref(), Some("v0.1.0-alpha.4"));
    }

    #[test]
    fn switch_to_errors_when_version_not_installed() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let err = store.switch_to("v9.9.9").unwrap_err();
        assert!(format!("{err:#}").contains("not installed"));
    }

    #[test]
    fn uninstall_refuses_active_version() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let src_dir = TempDir::new().unwrap();
        let src = fake_binary(src_dir.path(), "crap-cms");
        store.install_binary("v0.1.0-alpha.5", &src).unwrap();
        store.switch_to("v0.1.0-alpha.5").unwrap();

        let err = store.uninstall("v0.1.0-alpha.5").unwrap_err();
        assert!(format!("{err:#}").contains("cannot uninstall the active"));
    }

    #[test]
    fn uninstall_removes_inactive_version() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let src_dir = TempDir::new().unwrap();
        let src1 = fake_binary(src_dir.path(), "crap-cms1");
        let src2 = fake_binary(src_dir.path(), "crap-cms2");
        store.install_binary("v0.1.0-alpha.4", &src1).unwrap();
        store.install_binary("v0.1.0-alpha.5", &src2).unwrap();
        store.switch_to("v0.1.0-alpha.5").unwrap();

        store.uninstall("v0.1.0-alpha.4").unwrap();
        assert!(
            !store
                .installed()
                .unwrap()
                .contains(&"v0.1.0-alpha.4".to_string())
        );
    }

    #[test]
    fn owns_path_recognises_store_members() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let src_dir = TempDir::new().unwrap();
        let src = fake_binary(src_dir.path(), "crap-cms");
        let installed = store.install_binary("v0.1.0-alpha.5", &src).unwrap();

        assert!(store.owns_path(&installed));
        assert!(!store.owns_path(Path::new("/usr/bin/crap-cms")));
    }
}
