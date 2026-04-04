//! PID file management for the server process.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::commands::helpers;

/// Server PID filename.
const PID_FILENAME: &str = "crap.pid";

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

/// Check if a PID file exists and warn if the process is still running.
#[cfg(unix)]
pub fn check_existing_pid(config_dir: &Path) {
    helpers::check_existing_pid(config_dir, PID_FILENAME);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    #[cfg(unix)]
    fn check_existing_pid_no_file_no_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Should not panic
        check_existing_pid(tmp.path());
    }

    #[test]
    #[cfg(unix)]
    fn check_existing_pid_stale_pid_no_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Write a PID that almost certainly doesn't exist
        write_pid_file(tmp.path(), 999999999).unwrap();
        // Should not panic
        check_existing_pid(tmp.path());
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
        assert!(is_process_running(std::process::id()));
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
