//! Blueprint manifest — version metadata written to each saved blueprint.

use anyhow::{Context as _, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

/// Manifest filename written to each saved blueprint.
pub(super) const MANIFEST_FILENAME: &str = ".crap-blueprint.toml";

/// Metadata about a saved blueprint.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct BlueprintManifest {
    /// CMS version that created this blueprint.
    pub crap_version: String,
    /// ISO 8601 timestamp when the blueprint was saved.
    pub created_at: Option<String>,
}

impl BlueprintManifest {
    /// Create a new manifest for the current CMS version.
    pub fn new() -> Self {
        Self {
            crap_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: Some(Utc::now().to_rfc3339()),
        }
    }
}

/// Write a blueprint manifest file to the given directory.
pub(super) fn write_manifest(dir: &Path) -> Result<()> {
    let manifest = BlueprintManifest::new();
    let content =
        toml::to_string_pretty(&manifest).context("Failed to serialize blueprint manifest")?;

    fs::write(dir.join(MANIFEST_FILENAME), content)
        .with_context(|| format!("Failed to write manifest to '{}'", dir.display()))?;

    Ok(())
}

/// Read a blueprint manifest from the given directory. Returns `None` if the
/// manifest file does not exist (backward compatible with old blueprints).
pub(super) fn read_manifest(dir: &Path) -> Result<Option<BlueprintManifest>> {
    let path = dir.join(MANIFEST_FILENAME);

    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read manifest from '{}'", path.display()))?;
    let manifest: BlueprintManifest = toml::from_str(&content)
        .with_context(|| format!("Failed to parse manifest from '{}'", path.display()))?;

    Ok(Some(manifest))
}

/// Check the blueprint version against the running binary version.
///
/// Returns `None` if compatible, `Some(message)` on mismatch.
pub(super) fn check_blueprint_version(blueprint_version: &str) -> Option<String> {
    check_blueprint_version_against(blueprint_version, env!("CARGO_PKG_VERSION"))
}

/// Inner version check, takes explicit values for testability.
fn check_blueprint_version_against(blueprint_version: &str, pkg_version: &str) -> Option<String> {
    if blueprint_version == pkg_version {
        return None;
    }

    // Prefix match: "0.1" matches "0.1.0", "0.1.3", etc.
    if pkg_version.starts_with(blueprint_version)
        && pkg_version.as_bytes().get(blueprint_version.len()) == Some(&b'.')
    {
        return None;
    }

    Some(format!(
        "Blueprint was created with crap-cms v{}, but running version is v{}",
        blueprint_version, pkg_version
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_manifest(tmp.path()).unwrap();

        let manifest = read_manifest(tmp.path())
            .unwrap()
            .expect("manifest should exist");
        assert_eq!(manifest.crap_version, env!("CARGO_PKG_VERSION"));
        assert!(manifest.created_at.is_some());

        let content = fs::read_to_string(tmp.path().join(MANIFEST_FILENAME)).unwrap();
        assert!(content.contains("crap_version"));
        assert!(content.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn read_manifest_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = read_manifest(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_manifest_invalid_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join(MANIFEST_FILENAME), "not valid toml [[[").unwrap();

        let result = read_manifest(tmp.path());
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Failed to parse manifest")
        );
    }

    #[test]
    fn manifest_without_created_at() {
        let content = "crap_version = \"0.1.0\"\n";
        let manifest: BlueprintManifest = toml::from_str(content).unwrap();
        assert_eq!(manifest.crap_version, "0.1.0");
        assert!(manifest.created_at.is_none());
    }

    #[test]
    fn manifest_roundtrip_serialization() {
        let manifest = BlueprintManifest {
            crap_version: "1.2.3".to_string(),
            created_at: Some("2026-02-28T12:00:00+00:00".to_string()),
        };
        let serialized = toml::to_string_pretty(&manifest).unwrap();
        let deserialized: BlueprintManifest = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.crap_version, "1.2.3");
        assert_eq!(
            deserialized.created_at.as_deref(),
            Some("2026-02-28T12:00:00+00:00")
        );
    }

    #[test]
    fn check_version_exact_match() {
        assert!(check_blueprint_version_against("0.1.0", "0.1.0").is_none());
        assert!(check_blueprint_version_against("1.2.3", "1.2.3").is_none());
    }

    #[test]
    fn check_version_prefix_match() {
        assert!(check_blueprint_version_against("0.1", "0.1.0").is_none());
        assert!(check_blueprint_version_against("0.1", "0.1.5").is_none());
        assert!(check_blueprint_version_against("1", "1.2.3").is_none());
    }

    #[test]
    fn check_version_mismatch() {
        let msg = check_blueprint_version_against("0.2.0", "0.1.0");
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert!(msg.contains("0.2.0"));
        assert!(msg.contains("0.1.0"));

        assert!(check_blueprint_version_against("1.0.0", "0.1.0").is_some());
    }

    #[test]
    fn check_version_current() {
        assert!(check_blueprint_version(env!("CARGO_PKG_VERSION")).is_none());
    }

    #[test]
    fn check_version_no_false_prefix() {
        // "0.1" should NOT match "0.10.0" — prefix must be followed by a dot
        let msg = check_blueprint_version_against("0.1", "0.10.0");
        assert!(msg.is_some(), "0.1 should not match 0.10.0");
    }
}
