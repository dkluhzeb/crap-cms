//! Process lifecycle management — detach, stop, restart, status.

#[cfg(unix)]
use anyhow::bail;
use anyhow::{Context as _, Result};
use std::{env, path::Path, process};
#[cfg(unix)]
use std::{
    fs, thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
use tracing::debug;

use crate::cli;
#[cfg(unix)]
use crate::commands::helpers::send_signal;

use super::pid::write_pid_file;
#[cfg(unix)]
use super::pid::{check_existing_pid, is_process_running, read_pid, remove_pid_file};
use super::startup::{ServeMode, validate_config_dir};

/// Build the argument vector for the re-exec'd detached child.
///
/// Forwards every flag that changes the child's behavior — including
/// `--json`, which the parent consumes for its own (short-lived) logging
/// but the long-running child needs too, or its forced file logs come out
/// plain-text. Extracted (and not `tarpaulin`-excluded) so the forwarding
/// is unit-testable without spawning a process.
fn detach_child_args(
    config_dir: &Path,
    only: Option<ServeMode>,
    no_scheduler: bool,
    json: bool,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "-C".into(),
        config_dir.as_os_str().to_owned(),
        "serve".into(),
    ];

    if let Some(mode) = only {
        args.push("--only".into());
        args.push(
            match mode {
                ServeMode::Admin => "admin",
                ServeMode::Grpc => "grpc",
            }
            .into(),
        );
    }

    if no_scheduler {
        args.push("--no-scheduler".into());
    }

    if json {
        args.push("--json".into());
    }

    args
}

/// Re-exec the current binary as a detached background process.
///
/// # Errors
///
/// Returns an error if the executable path can't be determined, the config
/// directory is invalid, or the child process fails to spawn.
#[cfg(not(tarpaulin_include))]
pub fn detach(
    config_dir: &Path,
    only: Option<ServeMode>,
    no_scheduler: bool,
    json: bool,
) -> Result<()> {
    let exe = env::current_exe().context("Failed to determine executable path")?;

    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    validate_config_dir(&config_dir)?;

    #[cfg(unix)]
    check_existing_pid(&config_dir);

    let mut cmd = process::Command::new(&exe);

    cmd.args(detach_child_args(&config_dir, only, no_scheduler, json));

    // Tell the child it was detached so it can auto-enable file logging
    // (the child runs without --detach, so it can't detect this itself).
    cmd.env("_CRAP_DETACHED", "1");

    let child = cmd
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .spawn()
        .context("Failed to spawn detached process")?;

    let pid = child.id();

    write_pid_file(&config_dir, pid)?;

    cli::success(&format!("Started crap-cms in background (PID {pid})"));

    Ok(())
}

/// Poll `predicate` until it returns `false` or the timeout elapses.
///
/// Returns `true` iff the predicate transitioned to `false` before the
/// deadline, `false` iff the timeout elapsed first. The predicate is called
/// once immediately, then at each `poll_interval` until the deadline.
#[cfg(unix)]
fn wait_until_false<F>(timeout: Duration, poll_interval: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if !predicate() {
            return true;
        }

        thread::sleep(poll_interval);
    }

    !predicate()
}

/// Stop a running detached instance by sending SIGTERM, falling back to SIGKILL.
///
/// # Errors
///
/// Returns an error if the config dir is invalid, no PID file is found, the
/// process can't be signalled, or it fails to exit after both signals.
#[cfg(unix)]
pub fn stop(config_dir: &Path) -> Result<()> {
    validate_config_dir(config_dir)?;

    let pid = read_pid(config_dir).context(
        "No PID file found — is there a detached instance running?\n\
         Start one with: crap-cms serve --detach",
    )?;

    if !is_process_running(pid) {
        remove_pid_file(config_dir);

        bail!("Process {pid} is not running (stale PID file removed)");
    }

    // Send SIGTERM for graceful shutdown.
    send_signal(pid, libc::SIGTERM)?;

    // Wait for graceful shutdown (up to 10 seconds).
    let exited = wait_until_false(Duration::from_secs(10), Duration::from_millis(100), || {
        is_process_running(pid)
    });

    if exited {
        remove_pid_file(config_dir);

        cli::success(&format!("Stopped crap-cms (PID {pid})"));

        return Ok(());
    }

    // Still running — force kill.
    cli::warning(&format!(
        "Process {pid} did not stop within 10s, sending SIGKILL"
    ));

    let _ = send_signal(pid, libc::SIGKILL);

    // Brief wait for the force kill to take effect.
    thread::sleep(Duration::from_millis(500));

    remove_pid_file(config_dir);

    cli::success(&format!("Force-stopped crap-cms (PID {pid})"));

    Ok(())
}

/// Restart a detached instance: stop the current one, then start a new one.
///
/// # Errors
///
/// Returns an error if the config dir is invalid or the detach step fails.
/// The stop step's error is non-fatal: a stale PID file is cleaned up and
/// `detach` proceeds.
#[cfg(unix)]
pub fn restart(
    config_dir: &Path,
    only: Option<ServeMode>,
    no_scheduler: bool,
    json: bool,
) -> Result<()> {
    validate_config_dir(config_dir)?;

    // Stop if running — tolerate "not running" errors (race between check and kill).
    if let Some(pid) = read_pid(config_dir) {
        if is_process_running(pid) {
            if let Err(e) = stop(config_dir) {
                // Process may have exited between check and stop — not an error.
                debug!("stop() during restart: {e}");
            }
        } else {
            remove_pid_file(config_dir);
        }
    }

    detach(config_dir, only, no_scheduler, json)
}

/// Show the status of a detached instance.
///
/// # Errors
///
/// Returns an error if the config directory is invalid.
#[cfg(unix)]
pub fn status(config_dir: &Path) -> Result<()> {
    validate_config_dir(config_dir)?;

    let Some(pid) = read_pid(config_dir) else {
        cli::info("Not running (no PID file)");

        return Ok(());
    };

    if !is_process_running(pid) {
        remove_pid_file(config_dir);
        cli::info("Not running (stale PID file removed)");

        return Ok(());
    }

    cli::success(&format!("Running (PID {pid})"));

    // Try to show uptime from /proc on Linux.
    #[cfg(target_os = "linux")]
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        show_uptime(&stat);
    }

    Ok(())
}

/// Parse process start time from /proc/[pid]/stat and print uptime.
///
/// Uses `/proc/uptime` for system uptime and `/proc/[pid]/stat` field 22
/// (starttime in clock ticks). `CLK_TCK` is read from `getconf CLK_TCK`.
#[cfg(target_os = "linux")]
fn show_uptime(stat: &str) {
    // Field 22 is starttime in clock ticks since boot.
    // Fields after ") " (skipping pid and comm which may contain spaces).
    let fields: Vec<&str> = stat
        .rsplit(')')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();

    // Field 22 is at index 19 in the post-comm fields.
    let Some(start_ticks) = fields.get(19).and_then(|s| s.parse::<u64>().ok()) else {
        return;
    };

    // Get CLK_TCK via getconf (avoids libc dependency).
    let clk_tck: u64 = process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(100); // 100 is the default on Linux

    let Ok(uptime_str) = fs::read_to_string("/proc/uptime") else {
        return;
    };

    // Parse the integer seconds part of `/proc/uptime` so we can do the
    // arithmetic without dragging f64 through (avoids clippy's
    // cast_precision_loss / cast_possible_truncation chain).
    let Some(system_uptime_secs) = uptime_str
        .split_whitespace()
        .next()
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return;
    };

    let process_start_secs = start_ticks / clk_tck;
    let uptime_secs = system_uptime_secs.saturating_sub(process_start_secs);

    cli::kv("Uptime", &format_duration(uptime_secs));
}

/// Format seconds into a human-readable duration string.
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use crate::commands::serve::pid::pid_file_path;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    #[cfg(unix)]
    fn stop_no_pid_file_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        let err = stop(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("No PID file"));
    }

    #[test]
    #[cfg(unix)]
    fn stop_stale_pid_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        write_pid_file(tmp.path(), 999_999_999).unwrap();

        let err = stop(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("not running"));
        // PID file should be cleaned up
        assert!(!pid_file_path(tmp.path()).exists());
    }

    #[test]
    #[cfg(unix)]
    fn status_no_pid_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        // Should not error — just prints "Not running"
        status(tmp.path()).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn status_stale_pid_cleans_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        write_pid_file(tmp.path(), 999_999_999).unwrap();

        status(tmp.path()).unwrap();
        // Stale PID file should be removed
        assert!(!pid_file_path(tmp.path()).exists());
    }

    #[test]
    #[cfg(unix)]
    fn wait_until_false_returns_true_when_predicate_flips() {
        let counter = AtomicU32::new(0);
        let exited = wait_until_false(Duration::from_secs(5), Duration::from_millis(10), || {
            counter.fetch_add(1, Ordering::SeqCst) < 3
        });
        assert!(exited, "predicate flipped false, should return true");
    }

    #[test]
    #[cfg(unix)]
    fn wait_until_false_returns_false_when_predicate_stays_true() {
        // Simulate a process that never responds to SIGTERM — the 10s wait
        // must elapse so the caller issues SIGKILL. We use a short timeout
        // in the test so it runs quickly.
        let exited = wait_until_false(
            Duration::from_millis(100),
            Duration::from_millis(10),
            || true,
        );
        assert!(!exited, "predicate never flipped, should return false");
    }

    #[test]
    fn detach_forwards_json_flag() {
        let args = detach_child_args(Path::new("/cfg"), None, false, true);
        assert!(
            args.iter().any(|a| a == "--json"),
            "--json must be forwarded to the detached child: {args:?}"
        );
    }

    #[test]
    fn detach_omits_json_flag_when_off() {
        let args = detach_child_args(Path::new("/cfg"), None, false, false);
        assert!(!args.iter().any(|a| a == "--json"));
    }

    #[test]
    fn detach_forwards_only_and_no_scheduler() {
        let args = detach_child_args(Path::new("/cfg"), Some(ServeMode::Grpc), true, false);
        assert!(args.iter().any(|a| a == "-C"));
        assert!(args.iter().any(|a| a == "serve"));
        assert!(args.iter().any(|a| a == "--only"));
        assert!(args.iter().any(|a| a == "grpc"));
        assert!(args.iter().any(|a| a == "--no-scheduler"));
    }

    #[test]
    fn format_duration_seconds_only() {
        assert_eq!(format_duration(45), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(125), "2m 5s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3661), "1h 1m 1s");
    }

    #[test]
    fn format_duration_days() {
        assert_eq!(format_duration(90061), "1d 1h 1m 1s");
    }
}
