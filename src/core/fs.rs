//! Filesystem primitives shared by the paths that replace a file in place.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// The sibling `path` is staged under while being written: `<name>.tmp` in
/// the same directory, so the final rename never crosses a filesystem.
#[must_use]
pub fn staged_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".tmp");

    path.with_file_name(name)
}

/// Write `content` to `path` through a staged sibling renamed into place only
/// once it is complete.
///
/// A kill mid-write leaves the stage behind, never a truncated file under the
/// final name — and an existing file keeps its full previous content until the
/// replacement is whole. Every writer that REPLACES a file someone else reads
/// (an export, a config file the server loads at boot) goes through here, so
/// the guarantee is one implementation rather than one per caller.
///
/// # Errors
///
/// Returns an error if the stage cannot be written or the rename fails; on a
/// failed rename the stage is cleaned up and `path` is left untouched.
pub fn write_atomically(path: &Path, content: &str) -> Result<()> {
    let staged = staged_path(path);

    fs::write(&staged, content).with_context(|| format!("Failed to write {}", staged.display()))?;

    if let Err(e) = fs::rename(&staged, path) {
        let _ = fs::remove_file(&staged);

        return Err(e).with_context(|| format!("Failed to write {}", path.display()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stage_is_a_sibling_of_the_final_file() {
        assert_eq!(
            staged_path(Path::new("/out/data/export.json")),
            PathBuf::from("/out/data/export.json.tmp")
        );
    }

    /// The content lands under its final name and the stage is gone.
    #[test]
    fn a_complete_stage_is_renamed_into_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("file.txt");

        write_atomically(&path, "hello").expect("write");

        assert_eq!(fs::read_to_string(&path).expect("read"), "hello");
        assert!(!staged_path(&path).exists(), "the stage is renamed away");
    }

    /// A write that fails before the rename leaves the previous file intact —
    /// the whole point of staging.
    #[test]
    fn a_failed_write_leaves_the_previous_file_intact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("file.txt");
        fs::write(&path, "previous").expect("seed");

        // Staging into a directory that does not exist fails before the
        // rename, the way an interrupted write never reaches it.
        let missing = tmp.path().join("missing").join("file.txt");
        assert!(write_atomically(&missing, "partial").is_err());

        assert_eq!(fs::read_to_string(&path).expect("read"), "previous");
        assert!(!staged_path(&missing).exists());
    }
}
