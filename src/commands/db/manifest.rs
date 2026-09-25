//! Shared `manifest.json` shape for the `backup` / `restore` commands.

use anyhow::{Context as _, Result, bail};
use semver::Version;
use serde::{Deserialize, Serialize};

/// Structural version of the backup format. Bump ONLY on a
/// backward-incompatible change to the manifest/layout. `restore` refuses a
/// backup whose `format_version` is newer than this binary understands (it
/// cannot know how to read a future layout); a backup with an equal-or-older
/// version — including a pre-versioning backup that omits the field (→ 1) — is
/// accepted. This is the gate that lets the format evolve without silently
/// misreading old or future backups.
pub(super) const BACKUP_FORMAT_VERSION: u32 = 1;

fn default_format_version() -> u32 {
    1
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct BackupManifest {
    /// Backup-format structural version (see [`BACKUP_FORMAT_VERSION`]).
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    pub crap_version: String,
    pub timestamp: String,
    pub db_size: u64,
    #[serde(default)]
    pub uploads_size: Option<u64>,
    pub include_uploads: bool,
    pub source_db: String,
    pub source_config: String,
    /// Whether the backup carries the generated auth secret (`jwt_secret`).
    #[serde(default)]
    pub includes_secret: bool,
}

/// How the crap-cms that wrote a backup relates to the one restoring it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BackupOrigin {
    /// Written by this very version.
    SameVersion,
    /// Written by an older crap-cms: the next start schema-migrates it forward.
    OlderVersion,
}

/// Compare the backup's `crap_version` with this binary's.
///
/// A backup from a *newer* crap-cms is refused: restoring it is a downgrade.
/// The database carries schema, system columns and one-time migration gates
/// (versioned `_crap_meta` values) this binary does not know — it would treat
/// newer columns as orphans and re-run its older one-time passes over data a
/// newer computation already wrote. The newer binary must restore it.
///
/// # Errors
///
/// Returns an error when either version is not valid semver, or when the
/// backup is newer than `binary`.
pub(super) fn check_backup_origin(backup: &str, binary: &str) -> Result<BackupOrigin> {
    let backup_v = Version::parse(backup)
        .with_context(|| format!("manifest.json names an invalid crap-cms version {backup:?}"))?;
    let binary_v = Version::parse(binary)
        .with_context(|| format!("invalid crap-cms binary version {binary:?}"))?;

    if backup_v > binary_v {
        bail!(
            "This backup was taken with crap-cms {backup}, newer than this binary ({binary}). \
             Restoring it would downgrade the database, which an older crap-cms cannot \
             read safely — restore it with crap-cms {backup} or later \
             (`crap-cms update use v{backup}`)."
        );
    }

    if backup_v == binary_v {
        return Ok(BackupOrigin::SameVersion);
    }

    Ok(BackupOrigin::OlderVersion)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_fields() {
        let m = BackupManifest {
            format_version: BACKUP_FORMAT_VERSION,
            crap_version: "0.1.0-alpha.9".into(),
            timestamp: "2026-05-03T10:00:00+02:00".into(),
            db_size: 1024,
            uploads_size: Some(2048),
            include_uploads: true,
            source_db: "/tmp/crap.db".into(),
            source_config: "/tmp/config".into(),
            includes_secret: true,
        };

        let s = serde_json::to_string_pretty(&m).unwrap();
        let back: BackupManifest = serde_json::from_str(&s).unwrap();

        assert_eq!(back.format_version, m.format_version);
        assert_eq!(back.crap_version, m.crap_version);
        assert_eq!(back.timestamp, m.timestamp);
        assert_eq!(back.db_size, m.db_size);
        assert_eq!(back.uploads_size, m.uploads_size);
        assert_eq!(back.include_uploads, m.include_uploads);
        assert_eq!(back.source_db, m.source_db);
        assert_eq!(back.source_config, m.source_config);
        assert_eq!(back.includes_secret, m.includes_secret);
    }

    #[test]
    fn missing_uploads_size_deserializes_to_none() {
        let raw = r#"{
            "crap_version": "x",
            "timestamp": "t",
            "db_size": 1,
            "include_uploads": false,
            "source_db": "a",
            "source_config": "b"
        }"#;
        let m: BackupManifest = serde_json::from_str(raw).unwrap();
        assert!(m.uploads_size.is_none());
        assert!(!m.includes_secret, "an older backup carries no secret");
    }

    /// A pre-versioning manifest (no `format_version`) defaults to 1 so old
    /// backups still restore.
    #[test]
    fn missing_format_version_defaults_to_one() {
        let raw = r#"{
            "crap_version": "x",
            "timestamp": "t",
            "db_size": 1,
            "include_uploads": false,
            "source_db": "a",
            "source_config": "b"
        }"#;
        let m: BackupManifest = serde_json::from_str(raw).unwrap();
        assert_eq!(m.format_version, 1);
    }

    /// Regression: a backup from a newer crap-cms was accepted with a message
    /// promising a forward migration — restoring it is a downgrade.
    #[test]
    fn backup_from_a_newer_version_is_refused() {
        let err = check_backup_origin("0.1.0-alpha.11", "0.1.0-alpha.10").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("newer than this binary"), "{msg}");

        assert!(check_backup_origin("0.2.0", "0.1.9").is_err());
        assert!(check_backup_origin("1.0.0", "1.0.0-rc.1").is_err());
    }

    #[test]
    fn backup_from_the_same_or_an_older_version_is_accepted() {
        assert_eq!(
            check_backup_origin("0.1.0-alpha.10", "0.1.0-alpha.10").unwrap(),
            BackupOrigin::SameVersion
        );
        assert_eq!(
            check_backup_origin("0.1.0-alpha.9", "0.1.0-alpha.10").unwrap(),
            BackupOrigin::OlderVersion
        );
    }

    #[test]
    fn backup_with_an_invalid_version_is_refused() {
        let err = check_backup_origin("latest", "0.1.0").unwrap_err();
        assert!(format!("{err:#}").contains("invalid crap-cms version"));
    }
}
