//! Shared `manifest.json` shape for the `backup` / `restore` commands.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct BackupManifest {
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
}
