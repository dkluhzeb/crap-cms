//! Cross-action helpers shared by the `db` subcommands.

use std::{io, process};

use anyhow::{Result, bail};

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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::io::ErrorKind;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt as _;

    use super::*;

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
