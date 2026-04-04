//! Process lifecycle management — detach, stop, restart, status.

use anyhow::{Context as _, Result, bail};
#[cfg(unix)]
use std::io;
use std::{
    env, fs,
    path::Path,
    process, thread,
    time::{Duration, Instant},
};
use tracing::debug;

use crate::cli;

use super::pid::{
    check_existing_pid, is_process_running, read_pid, remove_pid_file, write_pid_file,
};
use super::startup::{ServeMode, validate_config_dir};

/// Send a signal to a process by PID.
#[cfg(unix)]
fn send_signal(pid: u32, sig: i32) -> Result<()> {
    let pid_i32 = i32::try_from(pid).context("PID too large for i32")?;
    // SAFETY: kill(2) is safe to call with any pid/signal combination.
    let ret = unsafe { libc::kill(pid_i32, sig) };

    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
            .with_context(|| format!("Failed to send signal {sig} to PID {pid}"))
    }
}

/// Re-exec the current binary as a detached background process.
#[cfg(not(tarpaulin_include))]
pub fn detach(config_dir: &Path, only: Option<ServeMode>, no_scheduler: bool) -> Result<()> {
    let exe = env::current_exe().context("Failed to determine executable path")?;

    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    validate_config_dir(&config_dir)?;

    #[cfg(unix)]
    check_existing_pid(&config_dir);

    let mut cmd = process::Command::new(&exe);

    cmd.arg("-C").arg(&config_dir).arg("serve");

    if let Some(mode) = only {
        cmd.arg("--only");
        cmd.arg(match mode {
            ServeMode::Admin => "admin",
            ServeMode::Api => "api",
        });
    }

    if no_scheduler {
        cmd.arg("--no-scheduler");
    }

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

    cli::success(&format!("Started crap-cms in background (PID {})", pid));

    Ok(())
}

/// Stop a running detached instance by sending SIGTERM, falling back to SIGKILL.
#[cfg(unix)]
pub fn stop(config_dir: &Path) -> Result<()> {
    validate_config_dir(config_dir)?;

    let pid = read_pid(config_dir).context(
        "No PID file found — is there a detached instance running?\n\
         Start one with: crap-cms serve --detach",
    )?;

    if !is_process_running(pid) {
        remove_pid_file(config_dir);

        bail!("Process {} is not running (stale PID file removed)", pid);
    }

    // Send SIGTERM for graceful shutdown.
    send_signal(pid, libc::SIGTERM)?;

    // Wait for graceful shutdown (up to 10 seconds).
    let deadline = Instant::now() + Duration::from_secs(10);

    while Instant::now() < deadline {
        if !is_process_running(pid) {
            remove_pid_file(config_dir);

            cli::success(&format!("Stopped crap-cms (PID {pid})"));

            return Ok(());
        }

        thread::sleep(Duration::from_millis(100));
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
#[cfg(unix)]
pub fn restart(config_dir: &Path, only: Option<ServeMode>, no_scheduler: bool) -> Result<()> {
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

    detach(config_dir, only, no_scheduler)
}

/// Show the status of a detached instance.
#[cfg(unix)]
pub fn status(config_dir: &Path) -> Result<()> {
    validate_config_dir(config_dir)?;

    let pid = match read_pid(config_dir) {
        Some(pid) => pid,
        None => {
            cli::info("Not running (no PID file)");

            return Ok(());
        }
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
/// (starttime in clock ticks). CLK_TCK is read from `getconf CLK_TCK`.
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
    let start_ticks: u64 = match fields.get(19).and_then(|s| s.parse().ok()) {
        Some(v) => v,
        None => return,
    };

    // Get CLK_TCK via getconf (avoids libc dependency).
    let clk_tck: u64 = process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(100); // 100 is the default on Linux

    let uptime_str = match fs::read_to_string("/proc/uptime") {
        Ok(s) => s,
        Err(_) => return,
    };

    let system_uptime_secs: f64 = match uptime_str
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
    {
        Some(v) => v,
        None => return,
    };

    let process_start_secs = start_ticks as f64 / clk_tck as f64;
    let uptime_secs = (system_uptime_secs - process_start_secs).max(0.0) as u64;

    cli::kv("Uptime", &format_duration(uptime_secs));
}

/// Format seconds into a human-readable duration string.
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
    use crate::commands::serve::pid::pid_file_path;

    #[test]
    fn stop_no_pid_file_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        let err = stop(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("No PID file"));
    }

    #[test]
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
    fn status_no_pid_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        // Should not error — just prints "Not running"
        status(tmp.path()).unwrap();
    }

    #[test]
    fn status_stale_pid_cleans_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        write_pid_file(tmp.path(), 999_999_999).unwrap();

        status(tmp.path()).unwrap();
        // Stale PID file should be removed
        assert!(!pid_file_path(tmp.path()).exists());
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
