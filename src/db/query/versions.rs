//! Version-specific database operations for the `_versions_{slug}` table.

use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::core::collection::CollectionDefinition;
use crate::core::document::VersionSnapshot;

/// Build a JSON snapshot of a document's current state (fields + join data).
pub fn build_snapshot(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    doc: &crate::core::Document,
) -> Result<serde_json::Value> {
    let mut data = serde_json::Map::new();
    for (k, v) in &doc.fields {
        data.insert(k.clone(), v.clone());
    }
    // Hydrate join table data into the snapshot
    let mut doc_clone = doc.clone();
    super::hydrate_document(conn, slug, def, &mut doc_clone, None, None)?;
    for (k, v) in &doc_clone.fields {
        data.insert(k.clone(), v.clone());
    }
    if let Some(ref ts) = doc.created_at {
        data.insert("created_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    if let Some(ref ts) = doc.updated_at {
        data.insert("updated_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    Ok(serde_json::Value::Object(data))
}

/// Create a new version entry. Clears previous `_latest` flag, inserts new version.
pub fn create_version(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
    status: &str,
    snapshot: &serde_json::Value,
) -> Result<VersionSnapshot> {
    let table = format!("_versions_{}", slug);
    let id = nanoid::nanoid!();

    // Get the next version number
    let next_version: i64 = conn.query_row(
        &format!("SELECT COALESCE(MAX(_version), 0) + 1 FROM {} WHERE _parent = ?1", table),
        [parent_id],
        |row| row.get(0),
    ).context("Failed to get next version number")?;

    // Clear previous _latest flag
    conn.execute(
        &format!("UPDATE {} SET _latest = 0 WHERE _parent = ?1 AND _latest = 1", table),
        [parent_id],
    ).context("Failed to clear previous latest flag")?;

    // Insert new version
    let snapshot_str = serde_json::to_string(snapshot)
        .context("Failed to serialize snapshot")?;
    conn.execute(
        &format!(
            "INSERT INTO {} (id, _parent, _version, _status, _latest, snapshot) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            table
        ),
        rusqlite::params![id, parent_id, next_version, status, snapshot_str],
    ).context("Failed to insert version")?;

    Ok(VersionSnapshot {
        id,
        parent: parent_id.to_string(),
        version: next_version,
        status: status.to_string(),
        latest: true,
        snapshot: snapshot.clone(),
        created_at: None,
        updated_at: None,
    })
}

/// Find the latest version for a parent document.
pub fn find_latest_version(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let mut stmt = conn.prepare(
        &format!(
            "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE _parent = ?1 AND _latest = 1 LIMIT 1",
            table
        ),
    )?;
    let result = stmt.query_row([parent_id], |row| {
        let snapshot_str: String = row.get(5)?;
        Ok(VersionSnapshot {
            id: row.get(0)?,
            parent: row.get(1)?,
            version: row.get(2)?,
            status: row.get(3)?,
            latest: row.get::<_, i32>(4)? != 0,
            snapshot: serde_json::from_str(&snapshot_str).unwrap_or(serde_json::Value::Null),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    });

    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// List versions for a parent document, newest first.
pub fn list_versions(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
    limit: Option<i64>,
) -> Result<Vec<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let limit_clause = limit.map(|l| format!(" LIMIT {}", l)).unwrap_or_default();
    let mut stmt = conn.prepare(
        &format!(
            "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE _parent = ?1 ORDER BY _version DESC{}",
            table, limit_clause
        ),
    )?;
    let rows = stmt.query_map([parent_id], |row| {
        let snapshot_str: String = row.get(5)?;
        Ok(VersionSnapshot {
            id: row.get(0)?,
            parent: row.get(1)?,
            version: row.get(2)?,
            status: row.get(3)?,
            latest: row.get::<_, i32>(4)? != 0,
            snapshot: serde_json::from_str(&snapshot_str).unwrap_or(serde_json::Value::Null),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })?;
    let mut versions = Vec::new();
    for row in rows {
        versions.push(row?);
    }
    Ok(versions)
}

/// Find a specific version by its ID.
pub fn find_version_by_id(
    conn: &rusqlite::Connection,
    slug: &str,
    version_id: &str,
) -> Result<Option<VersionSnapshot>> {
    let table = format!("_versions_{}", slug);
    let mut stmt = conn.prepare(
        &format!(
            "SELECT id, _parent, _version, _status, _latest, snapshot, created_at, updated_at \
             FROM {} WHERE id = ?1 LIMIT 1",
            table
        ),
    )?;
    let result = stmt.query_row([version_id], |row| {
        let snapshot_str: String = row.get(5)?;
        Ok(VersionSnapshot {
            id: row.get(0)?,
            parent: row.get(1)?,
            version: row.get(2)?,
            status: row.get(3)?,
            latest: row.get::<_, i32>(4)? != 0,
            snapshot: serde_json::from_str(&snapshot_str).unwrap_or(serde_json::Value::Null),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    });

    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Restore a version snapshot back to the main table. Updates all regular columns
/// and join tables from the snapshot data. Creates a new version recording the restore.
pub fn restore_version(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    parent_id: &str,
    snapshot: &serde_json::Value,
    status: &str,
) -> Result<crate::core::Document> {
    // Extract flat field data from snapshot for the UPDATE
    let obj = snapshot.as_object()
        .ok_or_else(|| anyhow::anyhow!("Snapshot is not a JSON object"))?;

    let mut data: HashMap<String, String> = HashMap::new();
    let insert_from_snapshot = |data: &mut HashMap<String, String>, key: &str| {
        match obj.get(key) {
            Some(serde_json::Value::String(s)) => { data.insert(key.to_string(), s.clone()); }
            Some(serde_json::Value::Number(n)) => { data.insert(key.to_string(), n.to_string()); }
            Some(serde_json::Value::Bool(b)) => { data.insert(key.to_string(), b.to_string()); }
            Some(serde_json::Value::Null) | None => {
                // Field was empty/null in the snapshot — clear it in the main table
                data.insert(key.to_string(), String::new());
            }
            _ => {} // complex types (arrays/objects) handled via join tables
        }
    };
    for field in &def.fields {
        if field.field_type == crate::core::field::FieldType::Group {
            for sub in &field.fields {
                let key = format!("{}__{}", field.name, sub.name);
                insert_from_snapshot(&mut data, &key);
            }
            continue;
        }
        if !field.has_parent_column() {
            continue; // join-table fields handled separately below
        }
        insert_from_snapshot(&mut data, &field.name);
    }

    let doc = super::update(conn, slug, def, parent_id, &data, None)?;

    // Restore join table data from snapshot
    let mut join_data: HashMap<String, serde_json::Value> = HashMap::new();
    for field in &def.fields {
        if !field.has_parent_column() {
            if let Some(v) = obj.get(&field.name) {
                join_data.insert(field.name.clone(), v.clone());
            }
        }
    }
    if !join_data.is_empty() {
        super::save_join_table_data(conn, slug, def, parent_id, &join_data, None)?;
    }

    // Update status
    set_document_status(conn, slug, parent_id, status)?;

    // Create a new version for the restore
    create_version(conn, slug, parent_id, status, snapshot)?;

    Ok(doc)
}

/// Set the `_status` column on a document in the main table.
pub fn set_document_status(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    status: &str,
) -> Result<()> {
    conn.execute(
        &format!("UPDATE {} SET _status = ?1, updated_at = datetime('now') WHERE id = ?2", slug),
        rusqlite::params![status, id],
    ).with_context(|| format!("Failed to set _status on {}.{}", slug, id))?;
    Ok(())
}

/// Get the `_status` column from a document in the main table.
pub fn get_document_status(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
) -> Result<Option<String>> {
    let result = conn.query_row(
        &format!("SELECT _status FROM {} WHERE id = ?1", slug),
        [id],
        |row| row.get(0),
    );
    match result {
        Ok(status) => Ok(Some(status)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Delete oldest versions beyond the max_versions cap for a document.
pub fn prune_versions(
    conn: &rusqlite::Connection,
    slug: &str,
    parent_id: &str,
    max_versions: u32,
) -> Result<()> {
    if max_versions == 0 {
        return Ok(()); // unlimited
    }
    let table = format!("_versions_{}", slug);
    // Delete all versions beyond the cap, keeping the newest ones
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE _parent = ?1 AND id NOT IN (\
                SELECT id FROM {} WHERE _parent = ?1 ORDER BY _version DESC LIMIT ?2\
            )",
            table, table
        ),
        rusqlite::params![parent_id, max_versions],
    ).context("Failed to prune versions")?;
    Ok(())
}
