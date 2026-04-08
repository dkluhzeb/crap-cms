//! Version management service — restore and list operations.
//!
//! Consolidates version restore logic shared between admin (collection + global)
//! and gRPC handlers. Separate from `versions.rs` which handles internal snapshot
//! creation during write operations.

use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, Document, collection::GlobalDefinition, document::VersionSnapshot,
    },
    db::{DbConnection, query, query::helpers::global_table},
};

use super::ServiceError;

/// Restore a collection document to a specific version snapshot.
///
/// Finds the version, applies the snapshot to the document, adjusts ref counts,
/// and creates a new version record. Caller manages the transaction.
pub fn restore_collection_version(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    document_id: &str,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document, ServiceError> {
    let version = query::find_version_by_id(conn, slug, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let doc = query::restore_version(
        conn,
        slug,
        def,
        document_id,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    Ok(doc)
}

/// Restore a global document to a specific version snapshot.
///
/// Finds the version (using the global table name), applies the snapshot,
/// adjusts ref counts, and creates a new version record. Caller manages the transaction.
pub fn restore_global_version(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    version_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Document, ServiceError> {
    let gtable = global_table(slug);

    let version = query::find_version_by_id(conn, &gtable, version_id)?
        .ok_or_else(|| ServiceError::NotFound(format!("Version '{version_id}' not found")))?;

    let doc = query::restore_global_version(
        conn,
        slug,
        def,
        &version.snapshot,
        "published",
        locale_config,
    )?;

    Ok(doc)
}

/// List version snapshots for a document/global with pagination.
///
/// Returns `(versions, total_count)`. The `table` parameter is the collection slug
/// for collections or the global table name for globals.
pub fn list_versions(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<(Vec<VersionSnapshot>, i64), ServiceError> {
    let total = query::count_versions(conn, table, parent_id)?;
    let versions = query::list_versions(conn, table, parent_id, limit, offset)?;

    Ok((versions, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    use rusqlite::Connection;
    use serde_json::json;

    use crate::core::FieldDefinition;
    use crate::core::field::FieldType;

    fn setup_versioned_collection() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT
            );
            INSERT INTO posts (id, title) VALUES ('p1', 'Original Title');",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.versions = Some(crate::core::collection::VersionsConfig {
            drafts: false,
            max_versions: 0,
        });

        (conn, def)
    }

    #[test]
    fn list_versions_empty() {
        let (conn, _def) = setup_versioned_collection();
        let (versions, total) = list_versions(&conn, "posts", "p1", None, None).unwrap();
        assert_eq!(total, 0);
        assert!(versions.is_empty());
    }

    #[test]
    fn list_versions_with_data() {
        let (conn, _def) = setup_versioned_collection();
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 0, '{\"title\": \"V1\"}'),
                    ('v2', 'p1', 2, 'published', 1, '{\"title\": \"V2\"}');",
        )
        .unwrap();

        let (versions, total) = list_versions(&conn, "posts", "p1", None, None).unwrap();
        assert_eq!(total, 2);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 2, "should be newest first");
    }

    #[test]
    fn list_versions_pagination() {
        let (conn, _def) = setup_versioned_collection();
        conn.execute_batch(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 0, '{}'),
                    ('v2', 'p1', 2, 'published', 0, '{}'),
                    ('v3', 'p1', 3, 'published', 1, '{}');",
        )
        .unwrap();

        let (versions, total) = list_versions(&conn, "posts", "p1", Some(2), Some(0)).unwrap();
        assert_eq!(total, 3);
        assert_eq!(versions.len(), 2);
    }

    #[test]
    fn restore_collection_version_not_found() {
        let (conn, def) = setup_versioned_collection();
        let lc = LocaleConfig::default();
        let result = restore_collection_version(&conn, "posts", &def, "p1", "nonexistent", &lc);
        assert!(matches!(result, Err(ServiceError::NotFound(_))));
    }

    #[test]
    fn restore_collection_version_success() {
        let (conn, def) = setup_versioned_collection();
        let snapshot = json!({"title": "Restored Title"}).to_string();
        conn.execute(
            "INSERT INTO _versions_posts (id, _parent, _version, _status, _latest, snapshot) \
             VALUES ('v1', 'p1', 1, 'published', 1, ?1)",
            [&snapshot],
        )
        .unwrap();

        let lc = LocaleConfig::default();
        let doc = restore_collection_version(&conn, "posts", &def, "p1", "v1", &lc).unwrap();
        assert_eq!(doc.get_str("title"), Some("Restored Title"));
    }
}
