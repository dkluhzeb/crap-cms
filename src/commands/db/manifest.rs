//! Shared `manifest.json` shape for the `backup` / `restore` commands.

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
}
