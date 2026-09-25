//! Cross-action helpers shared by the `db` subcommands.

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::{
    fs::{self, DirBuilder},
    io,
    path::Path,
    process,
};

use anyhow::{Context as _, Result, bail};

/// Classify a `tar` invocation result. `Ok(())` = the archive was written or
/// extracted; `Err` names why it was not (non-zero exit, or `tar` missing or
/// unspawnable).
///
/// `backup --include-uploads` and `restore --include-uploads` both shell out to
/// `tar` for the same `uploads.tar.gz`, and neither may report success for a
/// step that did not happen — so both read the outcome through here.
pub(super) fn classify_tar_status(status: io::Result<process::ExitStatus>) -> Result<()> {
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("tar exited with status {s}"),
        Err(e) => bail!("tar not found or failed: {e}"),
    }
}

/// Create `path` (and missing parents) with the last component owner-only
/// (`0700` on unix). An existing directory is tightened to `0700` too.
///
/// Backups hold the database snapshot — password and API-key hashes, sealed
/// TOTP secrets — and possibly the generated auth secret, so they get the
/// same owner-only treatment as the secret file, whatever the umask or the
/// permissions of a shared `--output` directory.
///
/// # Errors
///
/// Returns an error when the directory cannot be created or restricted.
pub(super) fn create_private_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);

    builder
        .create(path)
        .with_context(|| format!("Failed to create {}", path.display()))?;

    restrict_to_owner(path, 0o700)
}

/// Set `path` to `mode` (unix only; a no-op elsewhere).
///
/// # Errors
///
/// Returns an error when the permissions cannot be changed.
pub(super) fn restrict_to_owner(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("Failed to restrict permissions of {}", path.display()))?;

    #[cfg(not(unix))]
    let _ = (path, mode);

    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::io::ErrorKind;
    #[cfg(unix)]
    use std::os::unix::{fs::PermissionsExt as _, process::ExitStatusExt as _};

    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_dir_is_owner_only_even_with_a_loose_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("shared");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();

        let dir = shared.join("backup-1");
        create_private_dir(&dir).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn tar_status_success_is_ok_failure_is_err() {
        assert!(classify_tar_status(Ok(process::ExitStatus::from_raw(0))).is_ok());

        // Exit code 1 → wait-status 256 on unix.
        let failed = classify_tar_status(Ok(process::ExitStatus::from_raw(256)));
        assert!(failed.is_err());

        let spawn_err = classify_tar_status(Err(io::Error::from(ErrorKind::NotFound)));
        assert!(spawn_err.is_err());
    }
}
