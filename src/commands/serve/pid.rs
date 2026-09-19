//! PID file management for the server process.

use anyhow::Result;
#[cfg(unix)]
use anyhow::bail;
use std::{
    fs,
    path::{Path, PathBuf},
    process,
};

use crate::commands::helpers;

/// Server PID filename (shared with the destructive db commands' guard).
const PID_FILENAME: &str = helpers::SERVER_PID_FILENAME;

/// Path to the server PID file.
#[cfg_attr(not(test), allow(dead_code))]
pub fn pid_file_path(config_dir: &Path) -> PathBuf {
    helpers::pid_file_path(config_dir, PID_FILENAME)
}

/// Write the server PID to the PID file.
pub fn write_pid_file(config_dir: &Path, pid: u32) -> Result<()> {
    helpers::write_pid_file(config_dir, PID_FILENAME, pid)
}

/// Remove the server PID file on clean shutdown.
pub fn remove_pid_file(config_dir: &Path) {
    helpers::remove_pid_file(config_dir, PID_FILENAME);
}

/// Read the server PID from the PID file.
#[cfg(unix)]
pub fn read_pid(config_dir: &Path) -> Option<u32> {
    helpers::read_pid(config_dir, PID_FILENAME)
}

/// Check if a process with the given PID is running.
#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    helpers::is_process_running(pid)
}

/// Refuse while the server PID file names a live process other than this one.
///
/// # Errors
///
/// Returns an error naming the live process when one holds the file.
pub fn refuse_if_server_running(config_dir: &Path) -> Result<()> {
    refuse_if_running(config_dir, PID_FILENAME)
}

/// Refuse while `<config_dir>/data/<filename>` names a live process other
/// than this one: two instances on one project would both serve, and the file
/// can only name one of them. A file naming THIS process is ours already — the
/// parent of a detached start writes the child's PID before the child claims
/// it — and one naming a dead process is stale and may be taken over.
///
/// # Errors
///
/// Returns an error naming the live process when one holds the file.
#[cfg(unix)]
pub fn refuse_if_running(config_dir: &Path, filename: &str) -> Result<()> {
    let Some(pid) = helpers::read_pid(config_dir, filename) else {
        return Ok(());
    };

    if pid == process::id() || !helpers::is_process_running(pid) {
        return Ok(());
    }

    bail!(
        "another crap-cms process (PID {pid}) already holds {} — stop it first, or remove \
         the file if that PID is not a crap-cms process",
        helpers::pid_file_path(config_dir, filename).display()
    )
}

/// Process liveness is a Unix probe; elsewhere a PID file is never a refusal.
#[cfg(not(unix))]
pub fn refuse_if_running(_config_dir: &Path, _filename: &str) -> Result<()> {
    Ok(())
}

/// A PID file held for as long as this process can serve: written when
/// claimed, removed when dropped.
///
/// The claim comes AFTER the bootstrap work (config, schema sync, `on_init`
/// hooks) has succeeded, and the drop runs on every exit path — a startup
/// that fails after the claim, a server error, a clean shutdown. A file left
/// behind naming a dead process made `--stop` report "not running" and let
/// `--restart` start a second instance beside a live one.
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    /// Claim `<config_dir>/data/<filename>` for this process.
    ///
    /// # Errors
    ///
    /// Returns an error when the file names a live process other than this
    /// one (see [`refuse_if_running`]), or when it cannot be written.
    pub fn claim(config_dir: &Path, filename: &str) -> Result<Self> {
        refuse_if_running(config_dir, filename)?;

        helpers::write_pid_file(config_dir, filename, process::id())?;

        Ok(Self {
            path: helpers::pid_file_path(config_dir, filename),
        })
    }

    /// Claim the server PID file for this process.
    ///
    /// # Errors
    ///
    /// See [`PidFile::claim`].
    pub fn claim_server(config_dir: &Path) -> Result<Self> {
        Self::claim(config_dir, PID_FILENAME)
    }

    /// Remove the file now rather than at the end of the scope — for a
    /// shutdown that ends in `process::exit`, which runs no destructors.
    pub fn release(self) {
        drop(self);
    }

    /// Whether the file still names this process. Another process that
    /// legitimately took the file over — after a stop removed ours — must
    /// keep it.
    fn names_this_process(&self) -> bool {
        fs::read_to_string(&self.path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .is_some_and(|pid| pid == process::id())
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if self.names_this_process() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use anyhow::bail;

    use super::*;

    /// The PID of a live process that is not this one: the test runner's
    /// parent, which outlives every test.
    #[cfg(unix)]
    fn a_live_foreign_pid() -> u32 {
        // SAFETY: getppid(2) takes no arguments and cannot fail.
        let ppid = unsafe { libc::getppid() };

        u32::try_from(ppid).expect("a parent PID")
    }

    #[test]
    fn pid_file_write_and_remove() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path();

        write_pid_file(config_dir, 12345).unwrap();

        let path = pid_file_path(config_dir);
        assert!(path.exists());
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "12345");

        remove_pid_file(config_dir);
        assert!(!path.exists());
    }

    #[test]
    fn pid_file_path_is_in_data_dir() {
        let path = pid_file_path(Path::new("/some/config"));
        assert_eq!(path, PathBuf::from("/some/config/data/crap.pid"));
    }

    #[test]
    fn remove_pid_file_noop_if_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Should not panic
        remove_pid_file(tmp.path());
    }

    #[test]
    fn refuse_if_running_passes_with_no_file() {
        let tmp = tempfile::tempdir().expect("tempdir");

        refuse_if_server_running(tmp.path()).expect("nothing holds the file");
    }

    #[test]
    #[cfg(unix)]
    fn refuse_if_running_passes_a_stale_pid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_pid_file(tmp.path(), 999999999).unwrap();

        refuse_if_server_running(tmp.path()).expect("a dead process holds nothing");
    }

    /// Regression: an existing live PID only produced a warning, so a second
    /// `serve` on the same project went on to bootstrap beside the first.
    #[test]
    #[cfg(unix)]
    fn refuse_if_running_refuses_a_live_foreign_pid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid = a_live_foreign_pid();
        write_pid_file(tmp.path(), pid).unwrap();

        let err = refuse_if_server_running(tmp.path()).unwrap_err();

        assert!(
            err.to_string().contains(&format!("PID {pid}")),
            "unexpected error: {err}"
        );
    }

    /// The parent of a detached start writes the child's own PID before the
    /// child claims the file: that file is the child's, not a rival's.
    #[test]
    #[cfg(unix)]
    fn refuse_if_running_passes_a_file_naming_this_process() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_pid_file(tmp.path(), process::id()).unwrap();

        refuse_if_server_running(tmp.path()).expect("our own PID is not a rival");
    }

    #[test]
    fn a_claim_writes_this_pid_and_the_drop_removes_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = pid_file_path(tmp.path());

        let pid_file = PidFile::claim_server(tmp.path()).expect("claim");

        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            process::id().to_string()
        );

        drop(pid_file);

        assert!(!path.exists(), "the drop removes the file");
    }

    /// Regression: a startup that failed after writing the PID file returned
    /// through `?` and left the file naming a process that was gone.
    #[test]
    fn a_failed_startup_after_the_claim_leaves_no_file() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let start = |config_dir: &Path| -> Result<()> {
            let _pid_file = PidFile::claim_server(config_dir)?;

            bail!("bootstrap failed")
        };

        assert!(start(tmp.path()).is_err());
        assert!(
            !pid_file_path(tmp.path()).exists(),
            "an early exit must not leave a PID file behind"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_claim_takes_over_a_stale_pid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_pid_file(tmp.path(), 999999999).unwrap();

        let _pid_file = PidFile::claim_server(tmp.path()).expect("a stale file is taken over");

        assert_eq!(
            fs::read_to_string(pid_file_path(tmp.path())).expect("read"),
            process::id().to_string()
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_claim_refuses_a_live_foreign_pid_and_leaves_its_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid = a_live_foreign_pid();
        write_pid_file(tmp.path(), pid).unwrap();

        assert!(PidFile::claim_server(tmp.path()).is_err());
        assert_eq!(
            fs::read_to_string(pid_file_path(tmp.path())).expect("read"),
            pid.to_string(),
            "the live process keeps its file"
        );
    }

    /// A file another process wrote after ours was removed is not ours to
    /// remove.
    #[test]
    fn the_drop_leaves_a_file_another_process_took_over() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = pid_file_path(tmp.path());

        let pid_file = PidFile::claim_server(tmp.path()).expect("claim");
        fs::write(&path, "424242").expect("another process writes its PID");

        drop(pid_file);

        assert_eq!(fs::read_to_string(&path).expect("read"), "424242");
    }

    #[test]
    #[cfg(unix)]
    fn read_pid_no_file_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_pid(tmp.path()).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn read_pid_valid_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_pid_file(tmp.path(), 42).unwrap();
        assert_eq!(read_pid(tmp.path()), Some(42));
    }

    #[test]
    #[cfg(unix)]
    fn read_pid_garbage_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = pid_file_path(tmp.path());
        let _ = fs::create_dir_all(path.parent().unwrap());
        fs::write(&path, "not-a-number").unwrap();
        assert!(read_pid(tmp.path()).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn is_process_running_current_pid() {
        assert!(is_process_running(process::id()));
    }

    #[test]
    #[cfg(unix)]
    fn is_process_running_bogus_pid() {
        assert!(!is_process_running(999_999_999));
    }

    #[test]
    #[cfg(unix)]
    fn is_process_running_u32_max_returns_false() {
        assert!(
            !is_process_running(u32::MAX),
            "u32::MAX should not be treated as a valid PID"
        );
    }
}
