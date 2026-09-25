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
//!
//! A version is *installed* only once its binary exists at the final path.
//! Every write of a store binary goes through [`StagedBinary`]: the bytes land
//! in a hidden partial file inside the destination version directory (same
//! filesystem), are fsynced, SHA256-verified, made executable, and only then
//! renamed over the final name — so an interrupted download or copy never
//! leaves a truncated binary behind, and reinstalling the active version
//! swaps the file atomically instead of truncating it in place.

use anyhow::{Context, Result, bail};
use nanoid::nanoid;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use super::{checksum, version::validate_tag};

/// Abstracts the per-user store so tests can point it at a temp directory.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// The default store: `$XDG_DATA_HOME/crap-cms/` (or `~/.local/share/crap-cms/`).
    ///
    /// # Errors
    ///
    /// Returns an error if neither `$XDG_DATA_HOME` nor `$HOME` is set.
    pub fn default_for_user() -> Result<Self> {
        let data_home = xdg_data_home().context("resolving $XDG_DATA_HOME / $HOME")?;
        Ok(Self::at(data_home.join("crap-cms")))
    }

    /// Build a store rooted at an explicit path (tests).
    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// Directory of one version. Refuses anything that is not a `v`-prefixed
    /// semver tag, so no caller can reach outside `versions/`.
    ///
    /// # Errors
    ///
    /// Returns an error when `version` is not a valid tag.
    pub fn version_dir(&self, version: &str) -> Result<PathBuf> {
        validate_tag(version)?;

        Ok(self.versions_dir().join(version))
    }

    #[must_use]
    pub fn version_path(&self, version: &str) -> PathBuf {
        self.versions_dir().join(version).join(binary_filename())
    }

    /// Path of the `current` symlink.
    #[must_use]
    pub fn current_link(&self) -> PathBuf {
        self.root.join("current")
    }

    /// List installed versions: directories under `versions/` that hold the
    /// binary. A directory left behind by an interrupted install (no binary,
    /// or only a partial file) is not an installed version.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the versions directory fails.
    pub fn installed(&self) -> Result<Vec<String>> {
        let dir = self.versions_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut names = Vec::new();
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let has_binary = entry.path().join(binary_filename()).is_file();

            if entry.file_type()?.is_dir()
                && has_binary
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }

        names.sort();
        Ok(names)
    }

    /// Return the version name currently pointed at by `current`, if any.
    #[must_use]
    pub fn active_version(&self) -> Option<String> {
        let link = self.current_link();
        let target = fs::read_link(&link).ok()?;
        // target = ".../versions/<VER>/crap-cms"
        let ver = target.parent()?.file_name()?.to_str()?.to_string();
        Some(ver)
    }

    /// Open a staging file for `<store>/versions/<version>/crap-cms`.
    ///
    /// Write the binary to [`StagedBinary::path`], then
    /// [`StagedBinary::commit`] it; dropping it uncommitted removes the
    /// partial file.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid version tag or when the version
    /// directory cannot be created.
    pub fn stage_binary(&self, version: &str) -> Result<StagedBinary> {
        let dest_dir = self.version_dir(version)?;
        let created_dir = !dest_dir.exists();

        fs::create_dir_all(&dest_dir)
            .with_context(|| format!("creating {}", dest_dir.display()))?;

        let partial = dest_dir.join(format!(".{}.partial-{}", binary_filename(), nanoid!(12)));

        Ok(StagedBinary {
            dest_dir,
            partial,
            created_dir,
            committed: false,
        })
    }

    /// Install a binary from `src_path` as `<store>/versions/<version>/crap-cms`
    /// through a [`StagedBinary`] (copy → fsync → verify → rename).
    ///
    /// # Errors
    ///
    /// Returns an error if staging, copying, verifying against
    /// `expected_sha256`, or the final rename fails.
    pub fn install_binary(
        &self,
        version: &str,
        src_path: &Path,
        expected_sha256: &str,
    ) -> Result<PathBuf> {
        let staged = self.stage_binary(version)?;

        fs::copy(src_path, staged.path()).with_context(|| {
            format!(
                "copying {} to {}",
                src_path.display(),
                staged.path().display()
            )
        })?;

        staged.commit(expected_sha256)
    }

    /// Flip the `current` symlink to point at `<version>/crap-cms`.
    ///
    /// Uses symlink-then-rename so the swap is atomic: readers of `current`
    /// either see the old target or the new target, never a missing file.
    ///
    /// # Errors
    ///
    /// Returns an error if the target version isn't installed, or if any of
    /// the filesystem operations (mkdir, symlink, rename) fails.
    pub fn switch_to(&self, version: &str) -> Result<()> {
        let target = self.version_dir(version)?.join(binary_filename());
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
    ///
    /// # Errors
    ///
    /// Returns an error if the version is currently active or if removing
    /// the version directory fails.
    pub fn uninstall(&self, version: &str) -> Result<()> {
        if self.active_version().as_deref() == Some(version) {
            bail!(
                "cannot uninstall the active version {version}: switch to another version first with `crap-cms update use <other>`"
            );
        }
        let dir = self.version_dir(version)?;
        if !dir.exists() {
            bail!("version {version} is not installed");
        }
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        Ok(())
    }

    /// Does the given path live inside this store's versions tree?
    #[must_use]
    pub fn owns_path(&self, path: &Path) -> bool {
        let versions_canonical = self.versions_dir().canonicalize().ok();
        let path_canonical = path.canonicalize().ok();
        match (versions_canonical, path_canonical) {
            (Some(root), Some(p)) => p.starts_with(&root),
            _ => false,
        }
    }
}

/// A store binary being written: a hidden partial file inside the destination
/// version directory. See the module docs for the commit protocol.
pub struct StagedBinary {
    dest_dir: PathBuf,
    partial: PathBuf,
    created_dir: bool,
    committed: bool,
}

impl StagedBinary {
    /// Where the caller writes the binary bytes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.partial
    }

    /// Fsync the staged bytes, verify their SHA256, make them executable and
    /// atomically rename them over the final binary path.
    ///
    /// # Errors
    ///
    /// Returns an error on a checksum mismatch or any filesystem failure; the
    /// partial file is removed and the final path is left untouched.
    pub fn commit(mut self, expected_sha256: &str) -> Result<PathBuf> {
        File::open(&self.partial)
            .and_then(|f| f.sync_all())
            .with_context(|| format!("syncing {}", self.partial.display()))?;

        checksum::verify(&self.partial, expected_sha256)?;
        set_executable(&self.partial)?;

        let dest = self.dest_dir.join(binary_filename());
        fs::rename(&self.partial, &dest).with_context(|| {
            format!("renaming {} to {}", self.partial.display(), dest.display())
        })?;

        self.committed = true;
        sync_dir(&self.dest_dir);

        Ok(dest)
    }
}

impl Drop for StagedBinary {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        let _ = fs::remove_file(&self.partial);

        // Only removes the directory when this staging created it and
        // nothing else landed in it.
        if self.created_dir {
            let _ = fs::remove_dir(&self.dest_dir);
        }
    }
}

/// Persist a rename in `dir` (best effort; not supported on every platform).
fn sync_dir(dir: &Path) {
    if cfg!(unix) {
        let _ = File::open(dir).and_then(|d| d.sync_all());
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
pub(super) fn binary_filename() -> &'static str {
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
    use std::io::{Read, Write};
    use tempfile::TempDir;

    fn fake_binary(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(b"fake binary contents").unwrap();
        p
    }

    fn install(store: &Store, version: &str, src: &Path) -> PathBuf {
        let hex = checksum::file_hex(src).unwrap();
        store.install_binary(version, src, &hex).unwrap()
    }

    #[test]
    fn install_writes_binary_and_is_listed() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());

        let src_dir = TempDir::new().unwrap();
        let src = fake_binary(src_dir.path(), "crap-cms");

        let dest = install(&store, "v0.1.0-alpha.5", &src);
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
        install(&store, "v0.1.0-alpha.4", &src1);
        install(&store, "v0.1.0-alpha.5", &src2);

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
        install(&store, "v0.1.0-alpha.5", &src);
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
        install(&store, "v0.1.0-alpha.4", &src1);
        install(&store, "v0.1.0-alpha.5", &src2);
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
        let installed = install(&store, "v0.1.0-alpha.5", &src);

        assert!(store.owns_path(&installed));
        assert!(!store.owns_path(Path::new("/usr/bin/crap-cms")));
    }

    #[test]
    fn checksum_mismatch_leaves_no_binary_and_no_installed_version() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let src_dir = TempDir::new().unwrap();
        let src = fake_binary(src_dir.path(), "crap-cms");

        let err = store
            .install_binary("v0.1.0", &src, &"0".repeat(64))
            .unwrap_err();

        assert!(format!("{err:#}").contains("checksum mismatch"));
        assert!(store.installed().unwrap().is_empty());
        assert!(
            !store.versions_dir().join("v0.1.0").exists(),
            "a failed first install must not leave its version dir behind"
        );
    }

    #[test]
    fn dropped_stage_removes_partial_and_is_not_installed() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());

        let staged = store.stage_binary("v0.1.0").unwrap();
        fs::write(staged.path(), b"half a bin").unwrap();
        let partial = staged.path().to_path_buf();
        drop(staged);

        assert!(!partial.exists());
        assert!(store.installed().unwrap().is_empty());
    }

    #[test]
    fn version_dir_without_binary_is_not_installed() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let dir = store.versions_dir().join("v0.1.0");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".crap-cms.partial-abc"), b"half").unwrap();

        assert!(store.installed().unwrap().is_empty());
        assert!(store.switch_to("v0.1.0").is_err());
    }

    #[test]
    fn failed_reinstall_keeps_the_existing_binary() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let src_dir = TempDir::new().unwrap();
        let src = fake_binary(src_dir.path(), "crap-cms");
        let installed = install(&store, "v0.1.0", &src);
        store.switch_to("v0.1.0").unwrap();

        let other = src_dir.path().join("other");
        fs::write(&other, b"a different, corrupt download").unwrap();
        let err = store
            .install_binary("v0.1.0", &other, &checksum::file_hex(&src).unwrap())
            .unwrap_err();

        assert!(format!("{err:#}").contains("checksum mismatch"));
        assert_eq!(fs::read(&installed).unwrap(), b"fake binary contents");
        assert_eq!(store.installed().unwrap(), vec!["v0.1.0".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn reinstall_replaces_the_binary_atomically() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().to_path_buf());
        let src_dir = TempDir::new().unwrap();
        let src = fake_binary(src_dir.path(), "crap-cms");
        let installed = install(&store, "v0.1.0", &src);
        let open_before = fs::File::open(&installed).unwrap();

        let newer = src_dir.path().join("newer");
        fs::write(&newer, b"new build").unwrap();
        install(&store, "v0.1.0", &newer);

        assert_eq!(fs::read(&installed).unwrap(), b"new build");

        // The old inode (e.g. a running server) is untouched: rename, not truncate.
        let mut old = String::new();
        (&open_before).read_to_string(&mut old).unwrap();
        assert_eq!(old, "fake binary contents");
    }

    #[test]
    fn store_operations_refuse_traversing_versions() {
        let tmp = TempDir::new().unwrap();
        let store = Store::at(tmp.path().join("store"));
        let victim = tmp.path().join("victim");
        fs::create_dir_all(&victim).unwrap();

        assert!(store.uninstall("v0.1.0/../../victim").is_err());
        assert!(store.switch_to("v0.1.0/../..").is_err());
        assert!(store.stage_binary("../escape").is_err());
        assert!(victim.exists());
    }
}
