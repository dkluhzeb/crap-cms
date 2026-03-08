/// Builder for [`VersionSnapshot`].
///
/// `id` and `parent` are taken in `new()`.
/// `version`, `status`, `latest`, and `snapshot` are required via chained methods.
use crate::core::document::VersionSnapshot;

pub struct VersionSnapshotBuilder {
    id: String,
    parent: String,
    version: Option<i64>,
    status: Option<String>,
    latest: Option<bool>,
    snapshot: Option<serde_json::Value>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

impl VersionSnapshotBuilder {
    pub fn new(id: impl Into<String>, parent: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            parent: parent.into(),
            version: None,
            status: None,
            latest: None,
            snapshot: None,
            created_at: None,
            updated_at: None,
        }
    }

    pub fn version(mut self, v: i64) -> Self {
        self.version = Some(v);
        self
    }

    pub fn status(mut self, s: impl Into<String>) -> Self {
        self.status = Some(s.into());
        self
    }

    pub fn latest(mut self, l: bool) -> Self {
        self.latest = Some(l);
        self
    }

    pub fn snapshot(mut self, s: serde_json::Value) -> Self {
        self.snapshot = Some(s);
        self
    }

    pub fn created_at(mut self, ts: impl Into<String>) -> Self {
        self.created_at = Some(ts.into());
        self
    }

    pub fn updated_at(mut self, ts: impl Into<String>) -> Self {
        self.updated_at = Some(ts.into());
        self
    }

    pub fn build(self) -> VersionSnapshot {
        VersionSnapshot {
            id: self.id,
            parent: self.parent,
            version: self
                .version
                .expect("VersionSnapshotBuilder: version is required"),
            status: self
                .status
                .expect("VersionSnapshotBuilder: status is required"),
            latest: self
                .latest
                .expect("VersionSnapshotBuilder: latest is required"),
            snapshot: self
                .snapshot
                .expect("VersionSnapshotBuilder: snapshot is required"),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_version_snapshot_with_all_fields() {
        let snap = VersionSnapshotBuilder::new("snap-1", "parent-1")
            .version(3)
            .status("published")
            .latest(true)
            .snapshot(serde_json::json!({"title": "v3"}))
            .created_at("2024-01-01")
            .updated_at("2024-01-02")
            .build();
        assert_eq!(snap.id, "snap-1");
        assert_eq!(snap.parent, "parent-1");
        assert_eq!(snap.version, 3);
        assert_eq!(snap.status, "published");
        assert!(snap.latest);
        assert_eq!(snap.snapshot, serde_json::json!({"title": "v3"}));
        assert_eq!(snap.created_at.as_deref(), Some("2024-01-01"));
        assert_eq!(snap.updated_at.as_deref(), Some("2024-01-02"));
    }

    #[test]
    #[should_panic(expected = "VersionSnapshotBuilder: version is required")]
    fn panics_without_version() {
        VersionSnapshotBuilder::new("s", "p")
            .status("draft")
            .latest(false)
            .snapshot(serde_json::json!({}))
            .build();
    }

    #[test]
    #[should_panic(expected = "VersionSnapshotBuilder: status is required")]
    fn panics_without_status() {
        VersionSnapshotBuilder::new("s", "p")
            .version(1)
            .latest(false)
            .snapshot(serde_json::json!({}))
            .build();
    }

    #[test]
    #[should_panic(expected = "VersionSnapshotBuilder: latest is required")]
    fn panics_without_latest() {
        VersionSnapshotBuilder::new("s", "p")
            .version(1)
            .status("draft")
            .snapshot(serde_json::json!({}))
            .build();
    }

    #[test]
    #[should_panic(expected = "VersionSnapshotBuilder: snapshot is required")]
    fn panics_without_snapshot() {
        VersionSnapshotBuilder::new("s", "p")
            .version(1)
            .status("draft")
            .latest(false)
            .build();
    }
}
