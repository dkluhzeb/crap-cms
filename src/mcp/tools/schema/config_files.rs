//! Config file tools: read, write, and list files within the config directory.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use serde_json::to_string_pretty;
use tracing::info;

/// Response shape for `write_config_file`: echoes the relative path written.
#[derive(Serialize)]
struct WrittenResponse<'a> {
    written: &'a str,
}

/// One entry in the `list_config_files` response.
#[derive(Serialize)]
struct ConfigFileEntry {
    name: String,
    #[serde(rename = "type")]
    kind: ConfigFileKind,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ConfigFileKind {
    File,
    Directory,
}

/// Safely resolve a relative path within the config directory.
/// Rejects absolute paths, `..` components, and symlinks escaping the boundary.
pub(in crate::mcp::tools) fn safe_config_path(
    config_dir: &Path,
    relative: &str,
) -> Result<PathBuf> {
    // Reject absolute paths outright (on Unix, Path::join with absolute replaces the base)
    if Path::new(relative).is_absolute() {
        bail!("Absolute paths not allowed");
    }
    // Reject .. traversal
    if relative.contains("..") {
        bail!("Path traversal not allowed");
    }
    let full_path = config_dir.join(relative);
    // Canonicalize and verify the result stays within config_dir.
    // For read/list, the file/dir must already exist for canonicalize to work.
    // For write, the parent must exist (create_dir_all handles this upstream).
    let canonical_base = config_dir
        .canonicalize()
        .with_context(|| format!("Config dir not found: {}", config_dir.display()))?;
    // If file exists, canonicalize it. Otherwise verify the parent is inside config_dir.
    if full_path.exists() {
        let canonical = full_path.canonicalize()?;

        if !canonical.starts_with(&canonical_base) {
            bail!("Path escapes config directory");
        }
    } else {
        // For new files, walk up the parent chain to find the nearest existing ancestor
        // and verify it stays within config_dir.
        let mut ancestor = full_path.parent();
        while let Some(p) = ancestor {
            if p.exists() {
                let canonical_ancestor = p.canonicalize()?;
                if !canonical_ancestor.starts_with(&canonical_base) {
                    bail!("Path escapes config directory");
                }
                break;
            }
            ancestor = p.parent();
        }
    }
    Ok(full_path)
}

/// Read a file from the config directory.
pub(in crate::mcp::tools) fn exec_read_config_file(
    path: &str,
    config_dir: &Path,
) -> Result<String> {
    let full_path = safe_config_path(config_dir, path)?;
    let content = fs::read_to_string(&full_path)
        .with_context(|| format!("Failed to read {}", full_path.display()))?;
    Ok(content)
}

/// Write a file to the config directory, creating parent directories as needed.
pub(in crate::mcp::tools) fn exec_write_config_file(
    path: &str,
    content: &str,
    config_dir: &Path,
    client_label: &str,
) -> Result<String> {
    let full_path = safe_config_path(config_dir, path)?;

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)?;
    }
    info!("MCP write_config_file: {} [client={}]", path, client_label);
    fs::write(&full_path, content)
        .with_context(|| format!("Failed to write {}", full_path.display()))?;
    Ok(to_string_pretty(&WrittenResponse { written: path })?)
}

/// List files and directories within a config subdirectory.
pub(in crate::mcp::tools) fn exec_list_config_files(
    subdir: Option<&str>,
    config_dir: &Path,
) -> Result<String> {
    let dir = match subdir {
        Some(s) if !s.is_empty() => safe_config_path(config_dir, s)?,
        _ => config_dir.to_path_buf(),
    };
    let mut files = Vec::new();

    if dir.is_dir() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = if entry.file_type()?.is_dir() {
                ConfigFileKind::Directory
            } else {
                ConfigFileKind::File
            };
            files.push(ConfigFileEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                kind,
            });
        }
    }
    Ok(to_string_pretty(&files)?)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use serde_json::{Value, from_str};

    use super::*;

    #[test]
    fn safe_config_path_rejects_absolute() {
        let dir = Path::new("/tmp");
        assert!(safe_config_path(dir, "/etc/passwd").is_err());
    }

    #[test]
    fn safe_config_path_rejects_dot_dot() {
        let dir = Path::new("/tmp");
        assert!(safe_config_path(dir, "../etc/passwd").is_err());
        assert!(safe_config_path(dir, "foo/../../etc/passwd").is_err());
    }

    #[test]
    fn safe_config_path_allows_relative() {
        let dir = std::env::temp_dir();
        // Should succeed — a simple relative path within an existing dir
        let result = safe_config_path(&dir, "test_file.txt");
        assert!(result.is_ok());
    }

    #[test]
    fn exec_read_config_file_success() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "world").unwrap();
        let result = exec_read_config_file("hello.txt", dir.path()).unwrap();
        assert_eq!(result, "world");
    }

    #[test]
    fn exec_read_config_file_nonexistent_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = exec_read_config_file("does_not_exist.txt", dir.path()).unwrap_err();
        assert!(err.to_string().contains("does_not_exist"));
    }

    #[test]
    fn exec_write_config_file_success() {
        let dir = tempfile::tempdir().unwrap();
        let result = exec_write_config_file("output.txt", "hello", dir.path(), "(test)").unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert_eq!(parsed["written"], "output.txt");
        let written = fs::read_to_string(dir.path().join("output.txt")).unwrap();
        assert_eq!(written, "hello");
    }

    #[test]
    fn exec_write_config_file_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            exec_write_config_file("subdir/nested/file.txt", "data", dir.path(), "(test)").unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert_eq!(parsed["written"], "subdir/nested/file.txt");
        let content = fs::read_to_string(dir.path().join("subdir/nested/file.txt")).unwrap();
        assert_eq!(content, "data");
    }

    #[test]
    fn exec_list_config_files_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "").unwrap();
        fs::write(dir.path().join("b.lua"), "").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let result = exec_list_config_files(None, dir.path()).unwrap();
        let files: Vec<Value> = from_str(&result).unwrap();
        assert!(files.len() >= 3);
        let names: Vec<&str> = files
            .iter()
            .map(|f| f["name"].as_str().unwrap_or(""))
            .collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.lua"));
        assert!(names.contains(&"sub"));
        let sub = files.iter().find(|f| f["name"] == "sub").unwrap();
        assert_eq!(sub["type"], "directory");
        let a = files.iter().find(|f| f["name"] == "a.txt").unwrap();
        assert_eq!(a["type"], "file");
    }

    #[test]
    fn exec_list_config_files_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("collections")).unwrap();
        fs::write(dir.path().join("collections/posts.lua"), "").unwrap();

        let result = exec_list_config_files(Some("collections"), dir.path()).unwrap();
        let files: Vec<Value> = from_str(&result).unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|f| f["name"].as_str().unwrap_or(""))
            .collect();
        assert!(names.contains(&"posts.lua"));
    }

    #[test]
    fn exec_list_config_files_nonexistent_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        // Subdir does not exist → safe_config_path succeeds (no traversal),
        // but the dir is not a directory so files is empty.
        let result = exec_list_config_files(Some("nonexistent"), dir.path()).unwrap();
        let files: Vec<Value> = from_str(&result).unwrap();
        assert!(files.is_empty());
    }
}
