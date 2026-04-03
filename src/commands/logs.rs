//! `crap-cms logs` command — view and manage log files.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};

use crate::{cli, commands::LogsAction, config::CrapConfig};

/// File name prefix used by `tracing-appender` for rotated log files.
const LOG_PREFIX: &str = "crap-cms.log";

/// Run the `logs` command.
pub fn run(
    config_dir: &Path,
    action: Option<LogsAction>,
    follow: bool,
    lines: usize,
) -> Result<()> {
    let config = CrapConfig::load(config_dir)?;
    let log_dir = config.log_dir(config_dir);

    if !log_dir.exists() {
        bail!(
            "Log directory does not exist: {}\n\
             Enable file logging in crap.toml:\n\n  \
             [logging]\n  \
             file = true",
            log_dir.display()
        );
    }

    match action {
        Some(LogsAction::Clear) => clear(&log_dir),
        None => tail(&log_dir, follow, lines),
    }
}

/// Prune old rotated log files, keeping the newest `max_files` files.
///
/// Called at serve startup and by `logs clear`. Files are sorted by name
/// (date-based names from `tracing-appender` sort chronologically).
pub fn prune_old_logs(log_dir: &Path, max_files: usize) -> Result<usize> {
    let mut log_files = list_log_files(log_dir)?;

    if log_files.len() <= max_files {
        return Ok(0);
    }

    // Sort ascending by name — oldest first.
    log_files.sort();

    let to_remove = log_files.len() - max_files;
    let mut removed = 0;

    for path in log_files.iter().take(to_remove) {
        if let Err(e) = fs::remove_file(path) {
            tracing::warn!("Failed to remove old log file {}: {}", path.display(), e);
        } else {
            removed += 1;
        }
    }

    Ok(removed)
}

/// Show the last N lines of the most recent log file, optionally following.
fn tail(log_dir: &Path, follow: bool, lines: usize) -> Result<()> {
    let log_file = find_latest_log(log_dir)?;

    let file = fs::File::open(&log_file)
        .with_context(|| format!("Failed to open {}", log_file.display()))?;

    let tail_content = tail_lines(&file, lines)?;

    print!("{tail_content}");

    if follow {
        follow_file(&log_file, file)?;
    }

    Ok(())
}

/// Clear old rotated log files, keeping only the current one.
fn clear(log_dir: &Path) -> Result<()> {
    let removed = prune_old_logs(log_dir, 1)?;

    if removed == 0 {
        cli::info("No old log files to remove");
    } else {
        cli::success(&format!("Removed {removed} old log file(s)"));
    }

    Ok(())
}

/// Read the last N lines from a file using seek-from-end.
///
/// Reads backward in chunks, collecting raw bytes into a list of chunks
/// (reversed at the end) to avoid O(n²) splice operations.
fn tail_lines(file: &fs::File, n: usize) -> Result<String> {
    let metadata = file.metadata()?;
    let file_size = metadata.len();

    if file_size == 0 {
        return Ok(String::new());
    }

    let mut reader = BufReader::new(file);
    let chunk_size: u64 = 8192;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut newline_count = 0usize;
    let mut pos = file_size;

    // Read backward in chunks, counting newlines as we go.
    loop {
        let read_start = pos.saturating_sub(chunk_size);
        let read_len = (pos - read_start) as usize;

        if read_len == 0 {
            break;
        }

        reader.seek(SeekFrom::Start(read_start))?;

        let mut buf = vec![0u8; read_len];

        reader.read_exact(&mut buf)?;

        newline_count += buf.iter().filter(|&&b| b == b'\n').count();
        chunks.push(buf);
        pos = read_start;

        // We need n+1 newlines to have n complete lines (or start of file).
        if newline_count > n || pos == 0 {
            break;
        }
    }

    // Reassemble chunks in forward order.
    chunks.reverse();
    let collected: Vec<u8> = chunks.into_iter().flatten().collect();

    let text = String::from_utf8_lossy(&collected);
    let all_lines: Vec<&str> = text.lines().collect();

    if all_lines.len() <= n {
        Ok(format!("{}\n", all_lines.join("\n")))
    } else {
        let start = all_lines.len() - n;

        Ok(format!("{}\n", all_lines[start..].join("\n")))
    }
}

/// Follow a log file, printing new content as it arrives.
fn follow_file(path: &Path, file: fs::File) -> Result<()> {
    let mut reader = BufReader::new(file);

    reader.seek(SeekFrom::End(0))?;

    let mut line = String::new();

    loop {
        line.clear();

        match reader.read_line(&mut line) {
            Ok(0) => {
                // No new data — check if file was rotated (new file with same prefix).
                thread::sleep(Duration::from_millis(200));

                // Check if the file was replaced (rotation). If the path now
                // points to a different inode or doesn't exist, reopen.
                if let Ok(new_meta) = fs::metadata(path)
                    && let Ok(cur_meta) = reader.get_ref().metadata()
                {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;

                        if new_meta.ino() != cur_meta.ino() {
                            // File was rotated — reopen.
                            let new_file = fs::File::open(path)?;
                            reader = BufReader::new(new_file);
                        }
                    }

                    #[cfg(not(unix))]
                    {
                        // On non-Unix, compare file sizes as a heuristic.
                        if new_meta.len() < cur_meta.len() {
                            let new_file = fs::File::open(path)?;
                            reader = BufReader::new(new_file);
                        }
                    }
                }
            }
            Ok(_) => {
                print!("{line}");
            }
            Err(e) => {
                return Err(e).context("Error reading log file");
            }
        }
    }
}

/// List all log files in the directory matching the log prefix.
fn list_log_files(log_dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(log_dir)
        .with_context(|| format!("Failed to read log directory {}", log_dir.display()))?;

    let mut files = Vec::new();

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_file()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with(LOG_PREFIX)
        {
            files.push(path);
        }
    }

    Ok(files)
}

/// Find the most recent log file in the directory.
fn find_latest_log(log_dir: &Path) -> Result<PathBuf> {
    let mut files = list_log_files(log_dir)?;

    if files.is_empty() {
        bail!(
            "No log files found in {}\n\
             Is file logging enabled? Add to crap.toml:\n\n  \
             [logging]\n  \
             file = true",
            log_dir.display()
        );
    }

    // Sort descending — newest file name last (date-based names sort naturally).
    files.sort();
    Ok(files.pop().expect("files is non-empty"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn tail_lines_empty_file() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let result = tail_lines(tmp.as_file(), 10).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn tail_lines_fewer_than_requested() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "line1").unwrap();
        writeln!(tmp, "line2").unwrap();
        tmp.flush().unwrap();

        let file = fs::File::open(tmp.path()).unwrap();
        let result = tail_lines(&file, 10).unwrap();
        assert_eq!(result, "line1\nline2\n");
    }

    #[test]
    fn tail_lines_exact_count() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        for i in 1..=5 {
            writeln!(tmp, "line{i}").unwrap();
        }
        tmp.flush().unwrap();

        let file = fs::File::open(tmp.path()).unwrap();
        let result = tail_lines(&file, 3).unwrap();
        assert_eq!(result, "line3\nline4\nline5\n");
    }

    #[test]
    fn tail_lines_single_line() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "only line").unwrap();
        tmp.flush().unwrap();

        let file = fs::File::open(tmp.path()).unwrap();
        let result = tail_lines(&file, 1).unwrap();
        assert_eq!(result, "only line\n");
    }

    #[test]
    fn list_log_files_filters_correctly() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("crap-cms.log"), "current").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-27"), "old").unwrap();
        fs::write(dir.path().join("other.txt"), "unrelated").unwrap();

        let files = list_log_files(dir.path()).unwrap();
        assert_eq!(files.len(), 2);

        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"crap-cms.log".to_string()));
        assert!(names.contains(&"crap-cms.log.2026-03-27".to_string()));
    }

    #[test]
    fn find_latest_log_returns_newest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-25"), "old").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-27"), "newest").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-26"), "middle").unwrap();

        let latest = find_latest_log(dir.path()).unwrap();
        assert_eq!(
            latest.file_name().unwrap().to_string_lossy(),
            "crap-cms.log.2026-03-27"
        );
    }

    #[test]
    fn find_latest_log_empty_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_latest_log(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn prune_old_logs_removes_oldest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-25"), "a").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-26"), "b").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-27"), "c").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-28"), "d").unwrap();

        let removed = prune_old_logs(dir.path(), 2).unwrap();
        assert_eq!(removed, 2);

        let remaining = list_log_files(dir.path()).unwrap();
        assert_eq!(remaining.len(), 2);

        let names: Vec<String> = remaining
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"crap-cms.log.2026-03-27".to_string()));
        assert!(names.contains(&"crap-cms.log.2026-03-28".to_string()));
    }

    #[test]
    fn prune_old_logs_noop_when_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-27"), "a").unwrap();

        let removed = prune_old_logs(dir.path(), 5).unwrap();
        assert_eq!(removed, 0);
    }

    #[test]
    fn clear_removes_all_but_one() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-25"), "a").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-26"), "b").unwrap();
        fs::write(dir.path().join("crap-cms.log.2026-03-27"), "c").unwrap();

        let removed = prune_old_logs(dir.path(), 1).unwrap();
        assert_eq!(removed, 2);

        let remaining = list_log_files(dir.path()).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(
            remaining[0].file_name().unwrap().to_string_lossy(),
            "crap-cms.log.2026-03-27"
        );
    }
}
