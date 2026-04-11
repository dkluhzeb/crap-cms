//! Version helpers — JSON mapping, sidebar data, missing relations, doc status.

use serde_json::{Value, json};
use tracing::error;

use crate::{
    core::{Document, FieldDefinition, Registry, document::VersionSnapshot},
    db::DbConnection,
    service::{
        ListVersionsInput, ServiceContext, document_info::find_missing_relations,
        find_version_by_id, list_versions,
    },
};

/// Map a `VersionSnapshot` to the JSON object used in templates.
pub fn version_to_json(v: VersionSnapshot) -> Value {
    json!({
        "id": v.id,
        "version": v.version,
        "status": v.status,
        "latest": v.latest,
        "created_at": v.created_at,
    })
}

/// Fetch the last N versions + total count for sidebar display.
/// Returns `(versions_json, total_count)`.
pub fn fetch_version_sidebar_data(ctx: &ServiceContext, parent_id: &str) -> (Vec<Value>, i64) {
    let input = ListVersionsInput::builder(parent_id).limit(Some(3)).build();

    match list_versions(ctx, &input) {
        Ok(result) => {
            let vers = result.docs.into_iter().map(version_to_json).collect();
            (vers, result.total)
        }
        Err(_) => (vec![], 0),
    }
}

/// Look up a version snapshot and find any missing relation targets.
/// Shared by collection and global restore confirm handlers.
pub fn load_version_with_missing_relations(
    ctx: &ServiceContext,
    conn: &dyn DbConnection,
    registry: &Registry,
    version_id: &str,
    fields: &[FieldDefinition],
) -> Result<(VersionSnapshot, Vec<crate::db::query::MissingRelation>), &'static str> {
    let version = match find_version_by_id(ctx, version_id) {
        Ok(Some(v)) => v,
        Ok(None) => return Err("Version not found"),
        Err(e) => {
            error!("Find version error: {}", e);
            return Err("Database error");
        }
    };

    let missing = find_missing_relations(conn, registry, &version.snapshot, fields);

    Ok((version, missing))
}

/// Extract the document's `_status` field for draft-enabled collections/globals.
/// Returns an empty string if drafts are not enabled.
pub fn extract_doc_status(document: &Document, has_drafts: bool) -> String {
    if has_drafts {
        document
            .fields
            .get("_status")
            .and_then(|v| v.as_str())
            .unwrap_or("published")
            .to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn version_to_json_maps_all_fields() {
        let v = VersionSnapshot::builder("v1", "doc1")
            .version(3)
            .status("published")
            .latest(true)
            .created_at("2026-01-01T00:00:00Z")
            .updated_at("2026-01-01T00:00:00Z")
            .snapshot(json!({}))
            .build();

        let json = version_to_json(v);
        assert_eq!(json["id"], "v1");
        assert_eq!(json["version"], 3);
        assert_eq!(json["status"], "published");
        assert_eq!(json["latest"], true);
        assert_eq!(json["created_at"], "2026-01-01T00:00:00Z");
    }
}
