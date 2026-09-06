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

/// TOML keys whose values are secrets. Kept in sync with the redacted-on-
/// `Serialize` newtypes that the `crap://config` resource relies on —
/// pinned by `redaction_list_covers_every_secret_newtype_key` below AND
/// by the sentinel partition in `tests/secret_redaction.rs` (which found
/// this list three keys behind the newtype set once: `redis_url`,
/// `rate_limit_redis_url`, `url`).
const SECRET_TOML_KEYS: &[&str] = &[
    "secret",
    "smtp_pass",
    "api_key",
    "secret_key",
    // URL-shaped credentials (RedisUrl / DbUrl newtypes).
    "redis_url",
    "rate_limit_redis_url",
    "url",
];

/// Section headers whose EVERY key/value pair is secret-bearing
/// (`WebhookHeaders` — values like `Authorization = "Bearer …"`).
const SECRET_TOML_SECTIONS: &[&str] = &["email.webhook_headers"];

/// Mask the values of known secret keys in raw `crap.toml` text so reading it
/// through the tool never surfaces the JWT secret, SMTP password, MCP `api_key`,
/// or S3 credentials — matching the `crap://config` resource, which redacts the
/// same fields via their `Serialize` impls. Comments and structure are
/// preserved; only the secret values are replaced.
fn redact_toml_secrets(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut in_secret_section = false;

    for line in content.lines() {
        let indent_len = line.len() - line.trim_start().len();
        let (indent, rest) = line.split_at(indent_len);

        if rest.starts_with('[') {
            let section = rest.trim().trim_matches(['[', ']']);
            in_secret_section = SECRET_TOML_SECTIONS.contains(&section);
        }

        if let Some((key, _value)) = rest.split_once('=')
            && !rest.starts_with('#')
            && (in_secret_section || SECRET_TOML_KEYS.contains(&key.trim()))
        {
            out.push_str(indent);
            out.push_str(key.trim());
            out.push_str(" = \"***REDACTED***\"");
        } else {
            out.push_str(line);
        }

        out.push('\n');
    }

    out
}

/// Read a file from the config directory. `crap.toml` is returned with its
/// secret values redacted (see [`redact_toml_secrets`]).
pub(in crate::mcp::tools) fn exec_read_config_file(
    path: &str,
    config_dir: &Path,
) -> Result<String> {
    let full_path = safe_config_path(config_dir, path)?;
    let content = fs::read_to_string(&full_path)
        .with_context(|| format!("Failed to read {}", full_path.display()))?;

    if full_path.file_name().is_some_and(|n| n == "crap.toml") {
        return Ok(redact_toml_secrets(&content));
    }

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
    /// the TOML redaction list is a second copy
    /// of the secret inventory and drifted once (three keys behind the
    /// newtype set). This pins every URL/credential key + the
    /// webhook-headers section.
    #[test]
    fn redaction_list_covers_every_secret_newtype_key() {
        let toml = r#"
[database]
url = "postgres://u:PGPW@h/db"

[auth]
secret = "JWTSECRET"
rate_limit_redis_url = "redis://u:RLPW@h"

[cache]
redis_url = "redis://u:CPW@h"

[email]
smtp_pass = "SMTPPW"

[email.webhook_headers]
Authorization = "Bearer WHTOKEN"
X-Api-Key = "WHKEY"

[mcp]
api_key = "MCPKEY0123"

[upload.s3]
secret_key = "S3SECRET"
"#;
        let redacted = super::redact_toml_secrets(toml);
        for secret in [
            "PGPW",
            "JWTSECRET",
            "RLPW",
            "CPW",
            "SMTPPW",
            "WHTOKEN",
            "WHKEY",
            "MCPKEY0123",
            "S3SECRET",
        ] {
            assert!(
                !redacted.contains(secret),
                "secret {secret} survived TOML redaction:\n{redacted}"
            );
        }
        // Non-secrets survive.
        assert!(redacted.contains("[database]"));
        assert!(redacted.contains("Authorization"));
    }

    use std::{fs, path::Path};

    use serde_json::{Value, from_str};

    use super::*;

    #[test]
    fn safe_config_path_rejects_absolute() {
        let dir = Path::new("/tmp");
        assert!(safe_config_path(dir, "/etc/passwd").is_err());
    }

    #[test]
    fn redact_toml_secrets_masks_known_keys_keeps_rest() {
        let toml = "\
[auth]
secret = \"super-secret-jwt\"

[email]
smtp_host = \"smtp.example.com\"
smtp_pass = \"hunter2\"

[mcp]
api_key = \"mcp-key-abc\"

[upload.s3]
secret_key = \"s3-secret\"
bucket = \"my-bucket\"
";
        let out = redact_toml_secrets(toml);

        // Secrets gone.
        assert!(!out.contains("super-secret-jwt"), "{out}");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("mcp-key-abc"), "{out}");
        assert!(!out.contains("s3-secret"), "{out}");
        assert_eq!(out.matches("***REDACTED***").count(), 4);

        // Non-secret values preserved.
        assert!(out.contains("smtp_host = \"smtp.example.com\""), "{out}");
        assert!(out.contains("bucket = \"my-bucket\""), "{out}");
        assert!(out.contains("[auth]"), "structure preserved: {out}");
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
