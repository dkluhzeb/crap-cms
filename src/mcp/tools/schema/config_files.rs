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
) -> Result<String> {
    let full_path = safe_config_path(config_dir, path)?;

    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent)?;
    }
    info!("MCP write_config_file: {}", path);
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
