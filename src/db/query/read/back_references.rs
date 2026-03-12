//! Scan all collections and globals for documents that reference a given target document.
//! Also: check version snapshots for missing (deleted) relationship targets.

use crate::config::LocaleConfig;
use crate::core::Registry;
use crate::core::field::{FieldDefinition, FieldType, to_title_case};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// A group of documents in one collection/global that reference a target via one field.
#[derive(Debug, Clone, Serialize)]
pub struct BackReference {
    pub owner_slug: String,
    pub owner_label: String,
    pub field_name: String,
    pub field_label: String,
    pub document_ids: Vec<String>,
    pub count: usize,
    pub is_global: bool,
}

impl BackReference {
    pub fn new(
        owner_slug: String,
        owner_label: String,
        field_name: String,
        field_label: String,
        document_ids: Vec<String>,
        is_global: bool,
    ) -> Self {
        let count = document_ids.len();
        Self {
            owner_slug,
            owner_label,
            field_name,
            field_label,
            document_ids,
            count,
            is_global,
        }
    }
}

/// Scan all collections and globals for back-references to `target_id` in `target_collection`.
pub fn find_back_references(
    conn: &rusqlite::Connection,
    registry: &Registry,
    target_collection: &str,
    target_id: &str,
    locale_config: &LocaleConfig,
) -> Vec<BackReference> {
    let mut results = Vec::new();

    // Scan collections
    for (slug, def) in &registry.collections {
        let table = slug.as_str();
        scan_fields(
            conn,
            &def.fields,
            table,
            table,
            def.display_name(),
            target_collection,
            target_id,
            locale_config,
            slug,
            false,
            "",
            &mut results,
        );
    }

    // Scan globals
    for (slug, def) in &registry.globals {
        let table = format!("_global_{}", slug);
        scan_fields(
            conn,
            &def.fields,
            &table,
            &table,
            def.display_name(),
            target_collection,
            target_id,
            locale_config,
            slug,
            true,
            "",
            &mut results,
        );
    }

    results
}

/// Recursively walk a field tree, matching the same recursion pattern as
/// `collect_column_specs_inner` in `src/db/migrate/helpers.rs`.
#[allow(clippy::too_many_arguments)]
fn scan_fields(
    conn: &rusqlite::Connection,
    fields: &[FieldDefinition],
    parent_table: &str,
    collection_table: &str,
    owner_label: &str,
    target_collection: &str,
    target_id: &str,
    locale_config: &LocaleConfig,
    owner_slug: &str,
    is_global: bool,
    prefix: &str,
    results: &mut Vec<BackReference>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                scan_fields(
                    conn,
                    &field.fields,
                    parent_table,
                    collection_table,
                    owner_label,
                    target_collection,
                    target_id,
                    locale_config,
                    owner_slug,
                    is_global,
                    &new_prefix,
                    results,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                scan_fields(
                    conn,
                    &field.fields,
                    parent_table,
                    collection_table,
                    owner_label,
                    target_collection,
                    target_id,
                    locale_config,
                    owner_slug,
                    is_global,
                    prefix,
                    results,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    scan_fields(
                        conn,
                        &tab.fields,
                        parent_table,
                        collection_table,
                        owner_label,
                        target_collection,
                        target_id,
                        locale_config,
                        owner_slug,
                        is_global,
                        prefix,
                        results,
                    );
                }
            }
            FieldType::Relationship | FieldType::Upload => {
                let rc = match &field.relationship {
                    Some(rc) if rc.all_collections().contains(&target_collection) => rc,
                    _ => continue,
                };
                let field_label = field_display_label(field);

                if field.has_parent_column() {
                    // Has-one: column on parent table
                    let col = if prefix.is_empty() {
                        field.name.clone()
                    } else {
                        format!("{}__{}", prefix, field.name)
                    };
                    let ids = query_has_one(
                        conn,
                        parent_table,
                        &col,
                        target_collection,
                        target_id,
                        rc.is_polymorphic(),
                        field.localized && locale_config.is_enabled(),
                        locale_config,
                        owner_slug,
                        is_global,
                    );
                    if !ids.is_empty() {
                        results.push(BackReference::new(
                            owner_slug.to_string(),
                            owner_label.to_string(),
                            field.name.clone(),
                            field_label,
                            ids,
                            is_global,
                        ));
                    }
                } else {
                    // Has-many: junction table
                    let junction = format!("{}_{}", collection_table, field.name);
                    let ids = query_has_many(
                        conn,
                        &junction,
                        target_collection,
                        target_id,
                        rc.is_polymorphic(),
                    );
                    if !ids.is_empty() {
                        results.push(BackReference::new(
                            owner_slug.to_string(),
                            owner_label.to_string(),
                            field.name.clone(),
                            field_label,
                            ids,
                            is_global,
                        ));
                    }
                }
            }
            FieldType::Array => {
                let array_table = format!("{}_{}", collection_table, field.name);
                scan_array_sub_fields(
                    conn,
                    &field.fields,
                    &array_table,
                    parent_table,
                    owner_label,
                    target_collection,
                    target_id,
                    owner_slug,
                    is_global,
                    &field.name,
                    results,
                );
            }
            FieldType::Blocks => {
                let blocks_table = format!("{}_{}", collection_table, field.name);
                scan_blocks(
                    conn,
                    &field.blocks,
                    &blocks_table,
                    owner_label,
                    target_collection,
                    target_id,
                    owner_slug,
                    is_global,
                    &field.name,
                    results,
                );
            }
            _ => {}
        }
    }
}

/// Query has-one relationship column for a reference.
#[allow(clippy::too_many_arguments)]
fn query_has_one(
    conn: &rusqlite::Connection,
    table: &str,
    col: &str,
    target_collection: &str,
    target_id: &str,
    is_polymorphic: bool,
    is_localized: bool,
    locale_config: &LocaleConfig,
    owner_slug: &str,
    is_global: bool,
) -> Vec<String> {
    if is_localized {
        // Localized has-one: check all locale columns
        let locale_cols: Vec<String> = locale_config
            .locales
            .iter()
            .map(|l| format!("{}__{}", col, l))
            .collect();
        if locale_cols.is_empty() {
            return Vec::new();
        }

        let match_value = if is_polymorphic {
            format!("{}/{}", target_collection, target_id)
        } else {
            target_id.to_string()
        };

        let conditions: Vec<String> = locale_cols
            .iter()
            .map(|c| format!("\"{}\" = ?1", c))
            .collect();
        let sql = format!(
            "SELECT id FROM \"{}\" WHERE {}",
            table,
            conditions.join(" OR ")
        );
        query_ids(
            conn,
            &sql,
            &[&match_value],
            owner_slug,
            target_id,
            is_global,
        )
    } else if is_polymorphic {
        let match_value = format!("{}/{}", target_collection, target_id);
        let sql = format!("SELECT id FROM \"{}\" WHERE \"{}\" = ?1", table, col);
        query_ids(
            conn,
            &sql,
            &[&match_value as &dyn rusqlite::types::ToSql],
            owner_slug,
            target_id,
            is_global,
        )
    } else {
        let sql = format!("SELECT id FROM \"{}\" WHERE \"{}\" = ?1", table, col);
        query_ids(
            conn,
            &sql,
            &[&target_id as &dyn rusqlite::types::ToSql],
            owner_slug,
            target_id,
            is_global,
        )
    }
}

/// Query has-many junction table for references.
fn query_has_many(
    conn: &rusqlite::Connection,
    junction_table: &str,
    target_collection: &str,
    target_id: &str,
    is_polymorphic: bool,
) -> Vec<String> {
    let sql = if is_polymorphic {
        format!(
            "SELECT DISTINCT parent_id FROM \"{}\" WHERE related_id = ?1 AND related_collection = ?2",
            junction_table
        )
    } else {
        format!(
            "SELECT DISTINCT parent_id FROM \"{}\" WHERE related_id = ?1",
            junction_table
        )
    };

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Back-ref scan skipping {}: {}", junction_table, e);
            return Vec::new();
        }
    };

    if is_polymorphic {
        match stmt.query_map(rusqlite::params![target_id, target_collection], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(r) => r.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    } else {
        match stmt.query_map(rusqlite::params![target_id], |row| row.get::<_, String>(0)) {
            Ok(r) => r.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Scan array sub-fields for relationship/upload fields (uses `flatten_array_sub_fields` logic).
#[allow(clippy::too_many_arguments)]
fn scan_array_sub_fields(
    conn: &rusqlite::Connection,
    fields: &[FieldDefinition],
    array_table: &str,
    _parent_table: &str,
    owner_label: &str,
    target_collection: &str,
    target_id: &str,
    owner_slug: &str,
    is_global: bool,
    array_field_name: &str,
    results: &mut Vec<BackReference>,
) {
    let flat = crate::core::field::flatten_array_sub_fields(fields);
    for sub in flat {
        match sub.field_type {
            FieldType::Relationship | FieldType::Upload => {
                let rc = match &sub.relationship {
                    Some(rc) if rc.all_collections().contains(&target_collection) => rc,
                    _ => continue,
                };

                if rc.has_many {
                    // Has-many inside array — junction table named {array_table}_{sub.name}
                    // This is unusual but theoretically possible. Skip for now.
                    continue;
                }

                let match_value = if rc.is_polymorphic() {
                    format!("{}/{}", target_collection, target_id)
                } else {
                    target_id.to_string()
                };

                let sql = format!(
                    "SELECT DISTINCT parent_id FROM \"{}\" WHERE \"{}\" = ?1",
                    array_table, sub.name
                );
                let ids = query_ids_simple(conn, &sql, &match_value);
                if !ids.is_empty() {
                    let label = format!(
                        "{} > {}",
                        to_title_case(array_field_name),
                        field_display_label(sub)
                    );
                    results.push(BackReference::new(
                        owner_slug.to_string(),
                        owner_label.to_string(),
                        format!("{}.{}", array_field_name, sub.name),
                        label,
                        ids,
                        is_global,
                    ));
                }
            }
            _ => {}
        }
    }
}

/// Scan blocks sub-fields for relationship/upload fields.
#[allow(clippy::too_many_arguments)]
fn scan_blocks(
    conn: &rusqlite::Connection,
    blocks: &[crate::core::field::BlockDefinition],
    blocks_table: &str,
    owner_label: &str,
    target_collection: &str,
    target_id: &str,
    owner_slug: &str,
    is_global: bool,
    blocks_field_name: &str,
    results: &mut Vec<BackReference>,
) {
    for block in blocks {
        let flat = crate::core::field::flatten_array_sub_fields(&block.fields);
        for sub in &flat {
            match sub.field_type {
                FieldType::Relationship | FieldType::Upload => {
                    let rc = match &sub.relationship {
                        Some(rc) if rc.all_collections().contains(&target_collection) => rc,
                        _ => continue,
                    };

                    if rc.has_many {
                        continue; // has-many inside blocks not supported for scan
                    }

                    let match_value = if rc.is_polymorphic() {
                        format!("{}/{}", target_collection, target_id)
                    } else {
                        target_id.to_string()
                    };

                    let json_path = format!("$.{}", sub.name);
                    let sql = format!(
                        "SELECT DISTINCT parent_id FROM \"{}\" WHERE _block_type = ?1 AND json_extract(data, ?2) = ?3",
                        blocks_table
                    );
                    let ids =
                        query_ids_blocks(conn, &sql, &block.block_type, &json_path, &match_value);
                    if !ids.is_empty() {
                        let label = format!(
                            "{} > {} > {}",
                            to_title_case(blocks_field_name),
                            block
                                .label
                                .as_ref()
                                .map(|l| l.resolve_default().to_string())
                                .unwrap_or_else(|| to_title_case(&block.block_type)),
                            field_display_label(sub),
                        );
                        results.push(BackReference::new(
                            owner_slug.to_string(),
                            owner_label.to_string(),
                            format!("{}.{}.{}", blocks_field_name, block.block_type, sub.name),
                            label,
                            ids,
                            is_global,
                        ));
                    }
                }
                _ => {}
            }
        }
    }
}

/// Get the display label for a field (admin label or title-cased name).
fn field_display_label(field: &FieldDefinition) -> String {
    field
        .admin
        .label
        .as_ref()
        .map(|l| l.resolve_default().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| to_title_case(&field.name))
}

/// Execute a query and collect `id` column values, filtering out self-references.
fn query_ids(
    conn: &rusqlite::Connection,
    sql: &str,
    params: &[&dyn rusqlite::types::ToSql],
    owner_slug: &str,
    target_id: &str,
    is_global: bool,
) -> Vec<String> {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            return Vec::new();
        }
    };
    let rows = match stmt.query_map(params, |row| row.get::<_, String>(0)) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            return Vec::new();
        }
    };
    rows.filter_map(|r| r.ok())
        // Skip self-references (same collection, same ID)
        .filter(|id| is_global || id != target_id || owner_slug != target_id)
        .collect()
}

/// Simple query for array/blocks parent_id lookups.
fn query_ids_simple(conn: &rusqlite::Connection, sql: &str, value: &str) -> Vec<String> {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            return Vec::new();
        }
    };
    let result = stmt.query_map([value], |row| row.get::<_, String>(0));
    match result {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Query blocks table with block_type + json_extract.
fn query_ids_blocks(
    conn: &rusqlite::Connection,
    sql: &str,
    block_type: &str,
    json_path: &str,
    value: &str,
) -> Vec<String> {
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            return Vec::new();
        }
    };
    let result = stmt.query_map(rusqlite::params![block_type, json_path, value], |row| {
        row.get::<_, String>(0)
    });
    match result {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => Vec::new(),
    }
}

// ── Missing relations in version snapshots ──────────────────────────

/// A field in a version snapshot that references documents which no longer exist.
#[derive(Debug, Clone, Serialize)]
pub struct MissingRelation {
    pub field_name: String,
    pub field_label: String,
    pub missing_ids: Vec<String>,
    pub missing_count: usize,
    pub total_ids: usize,
}

impl MissingRelation {
    pub fn new(
        field_name: String,
        field_label: String,
        missing_ids: Vec<String>,
        total_ids: usize,
    ) -> Self {
        let missing_count = missing_ids.len();
        Self {
            field_name,
            field_label,
            missing_ids,
            missing_count,
            total_ids,
        }
    }
}

/// Check a version snapshot for relationship/upload fields whose targets no longer exist.
pub fn find_missing_relations(
    conn: &rusqlite::Connection,
    registry: &Registry,
    snapshot: &serde_json::Value,
    fields: &[FieldDefinition],
) -> Vec<MissingRelation> {
    let obj = match snapshot.as_object() {
        Some(o) => o,
        None => return Vec::new(),
    };
    let mut results = Vec::new();
    collect_missing_fields(conn, registry, obj, fields, "", &mut results);
    results
}

/// Recursively walk the field tree and collect missing relations from the snapshot.
fn collect_missing_fields(
    conn: &rusqlite::Connection,
    registry: &Registry,
    obj: &serde_json::Map<String, serde_json::Value>,
    fields: &[FieldDefinition],
    prefix: &str,
    results: &mut Vec<MissingRelation>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                // Group snapshot can be flat (seo__title) or nested (seo: { title })
                if let Some(nested) = obj.get(&field.name).and_then(|v| v.as_object()) {
                    collect_missing_fields(
                        conn,
                        registry,
                        nested,
                        &field.fields,
                        &new_prefix,
                        results,
                    );
                } else {
                    collect_missing_fields(
                        conn,
                        registry,
                        obj,
                        &field.fields,
                        &new_prefix,
                        results,
                    );
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_missing_fields(conn, registry, obj, &field.fields, prefix, results);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_missing_fields(conn, registry, obj, &tab.fields, prefix, results);
                }
            }
            FieldType::Relationship | FieldType::Upload => {
                let rc = match &field.relationship {
                    Some(rc) => rc,
                    None => continue,
                };
                let key = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                let val = obj.get(&key).or_else(|| obj.get(&field.name));
                let ids = extract_ref_ids(val, rc.is_polymorphic());
                if ids.is_empty() {
                    continue;
                }
                let missing = check_ids_exist(conn, registry, &ids, rc);
                if !missing.is_empty() {
                    let label = field_display_label(field);
                    let total = ids.len();
                    results.push(MissingRelation::new(
                        field.name.clone(),
                        label,
                        missing.into_iter().collect(),
                        total,
                    ));
                }
            }
            FieldType::Array => {
                if let Some(arr) = obj.get(&field.name).and_then(|v| v.as_array()) {
                    collect_missing_in_array(
                        conn,
                        registry,
                        arr,
                        &field.fields,
                        &field.name,
                        results,
                    );
                }
            }
            FieldType::Blocks => {
                if let Some(arr) = obj.get(&field.name).and_then(|v| v.as_array()) {
                    collect_missing_in_blocks(
                        conn,
                        registry,
                        arr,
                        &field.blocks,
                        &field.name,
                        results,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Extract referenced IDs from a snapshot value.
fn extract_ref_ids(val: Option<&serde_json::Value>, is_polymorphic: bool) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    match val {
        Some(serde_json::Value::String(s)) if !s.is_empty() => {
            if let Some((col, id)) = parse_ref_id(s, is_polymorphic) {
                ids.push((col, id));
            }
        }
        Some(serde_json::Value::Array(arr)) => {
            for item in arr {
                if let Some(s) = item.as_str()
                    && !s.is_empty()
                    && let Some((col, id)) = parse_ref_id(s, is_polymorphic)
                {
                    ids.push((col, id));
                }
            }
        }
        _ => {}
    }
    ids
}

/// Parse a single reference ID string, returning (collection, id).
fn parse_ref_id(s: &str, is_polymorphic: bool) -> Option<(String, String)> {
    if is_polymorphic {
        let parts: Vec<&str> = s.splitn(2, '/').collect();
        if parts.len() == 2 {
            Some((parts[0].to_string(), parts[1].to_string()))
        } else {
            None // invalid polymorphic format
        }
    } else {
        // For non-polymorphic, collection is resolved later from RelationshipConfig
        Some((String::new(), s.to_string()))
    }
}

/// Check which IDs are missing from the database.
fn check_ids_exist(
    conn: &rusqlite::Connection,
    registry: &Registry,
    ids: &[(String, String)],
    rc: &crate::core::field::RelationshipConfig,
) -> HashSet<String> {
    // Group IDs by target collection
    let mut by_collection: HashMap<String, Vec<String>> = HashMap::new();
    for (col, id) in ids {
        let target = if col.is_empty() {
            rc.collection.clone()
        } else {
            col.clone()
        };
        by_collection.entry(target).or_default().push(id.clone());
    }

    let mut missing = HashSet::new();
    for (collection, check_ids) in &by_collection {
        // Verify the collection exists in the registry
        if registry.collections.get(collection).is_none() {
            // Collection doesn't exist at all — all IDs are missing
            for id in check_ids {
                let display = if rc.is_polymorphic() {
                    format!("{}/{}", collection, id)
                } else {
                    id.clone()
                };
                missing.insert(display);
            }
            continue;
        }
        let existing = query_existing_ids(conn, collection, check_ids);
        for id in check_ids {
            if !existing.contains(id) {
                let display = if rc.is_polymorphic() {
                    format!("{}/{}", collection, id)
                } else {
                    id.clone()
                };
                missing.insert(display);
            }
        }
    }
    missing
}

/// Query which IDs exist in a collection table.
fn query_existing_ids(
    conn: &rusqlite::Connection,
    collection: &str,
    ids: &[String],
) -> HashSet<String> {
    if ids.is_empty() {
        return HashSet::new();
    }
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT id FROM \"{}\" WHERE id IN ({})",
        collection,
        placeholders.join(", ")
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!("Missing relations check skipping {}: {}", collection, e);
            return HashSet::new();
        }
    };
    let params: Vec<&dyn rusqlite::types::ToSql> = ids
        .iter()
        .map(|s| s as &dyn rusqlite::types::ToSql)
        .collect();
    let result = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0));
    match result {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => HashSet::new(),
    }
}

/// Check array sub-fields for missing relations.
fn collect_missing_in_array(
    conn: &rusqlite::Connection,
    registry: &Registry,
    rows: &[serde_json::Value],
    fields: &[FieldDefinition],
    array_name: &str,
    results: &mut Vec<MissingRelation>,
) {
    let flat = crate::core::field::flatten_array_sub_fields(fields);
    for sub in flat {
        match sub.field_type {
            FieldType::Relationship | FieldType::Upload => {
                let rc = match &sub.relationship {
                    Some(rc) => rc,
                    None => continue,
                };
                let mut all_ids = Vec::new();
                for row in rows {
                    if let Some(obj) = row.as_object() {
                        let val = obj.get(&sub.name);
                        all_ids.extend(extract_ref_ids(val, rc.is_polymorphic()));
                    }
                }
                if all_ids.is_empty() {
                    continue;
                }
                let missing = check_ids_exist(conn, registry, &all_ids, rc);
                if !missing.is_empty() {
                    let label = format!(
                        "{} > {}",
                        to_title_case(array_name),
                        field_display_label(sub)
                    );
                    results.push(MissingRelation::new(
                        format!("{}.{}", array_name, sub.name),
                        label,
                        missing.into_iter().collect(),
                        all_ids.len(),
                    ));
                }
            }
            _ => {}
        }
    }
}

/// Check blocks sub-fields for missing relations.
fn collect_missing_in_blocks(
    conn: &rusqlite::Connection,
    registry: &Registry,
    rows: &[serde_json::Value],
    blocks: &[crate::core::field::BlockDefinition],
    blocks_name: &str,
    results: &mut Vec<MissingRelation>,
) {
    for block in blocks {
        let flat = crate::core::field::flatten_array_sub_fields(&block.fields);
        for sub in &flat {
            match sub.field_type {
                FieldType::Relationship | FieldType::Upload => {
                    let rc = match &sub.relationship {
                        Some(rc) => rc,
                        None => continue,
                    };
                    let mut all_ids = Vec::new();
                    for row in rows {
                        if let Some(obj) = row.as_object() {
                            let bt = obj
                                .get("_block_type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if bt == block.block_type {
                                let val = obj.get(&sub.name);
                                all_ids.extend(extract_ref_ids(val, rc.is_polymorphic()));
                            }
                        }
                    }
                    if all_ids.is_empty() {
                        continue;
                    }
                    let missing = check_ids_exist(conn, registry, &all_ids, rc);
                    if !missing.is_empty() {
                        let label = format!(
                            "{} > {} > {}",
                            to_title_case(blocks_name),
                            block
                                .label
                                .as_ref()
                                .map(|l| l.resolve_default().to_string())
                                .unwrap_or_else(|| to_title_case(&block.block_type)),
                            field_display_label(sub),
                        );
                        results.push(MissingRelation::new(
                            format!("{}.{}.{}", blocks_name, block.block_type, sub.name),
                            label,
                            missing.into_iter().collect(),
                            all_ids.len(),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::Registry;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{migrate, pool};

    fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    fn locale_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn setup_db(
        collections: &[CollectionDefinition],
        globals: &[GlobalDefinition],
        locale: &LocaleConfig,
    ) -> (tempfile::TempDir, crate::db::DbPool, Registry) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = crate::config::CrapConfig {
            database: crate::config::DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");

        let registry_shared = Registry::shared();
        {
            let mut reg = registry_shared.write().unwrap();
            for c in collections {
                reg.register_collection(c.clone());
            }
            for g in globals {
                reg.register_global(g.clone());
            }
        }
        migrate::sync_all(&db_pool, &registry_shared, locale).expect("sync");

        let registry = (*Registry::snapshot(&registry_shared)).clone();
        (tmp, db_pool, registry)
    }

    fn insert_doc(conn: &rusqlite::Connection, table: &str, id: &str) {
        conn.execute(&format!("INSERT INTO \"{}\" (id) VALUES (?1)", table), [id])
            .unwrap();
    }

    fn insert_doc_with_field(
        conn: &rusqlite::Connection,
        table: &str,
        id: &str,
        col: &str,
        val: &str,
    ) {
        conn.execute(
            &format!(
                "INSERT INTO \"{}\" (id, \"{}\") VALUES (?1, ?2)",
                table, col
            ),
            rusqlite::params![id, val],
        )
        .unwrap();
    }

    // ── Has-one relationship ──────────────────────────────────────────

    #[test]
    fn has_one_finds_back_reference() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");
        insert_doc_with_field(&conn, "posts", "p2", "image", "m1");
        insert_doc_with_field(&conn, "posts", "p3", "image", "other");

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].field_name, "image");
        assert_eq!(refs[0].count, 2);
        assert!(refs[0].document_ids.contains(&"p1".to_string()));
        assert!(refs[0].document_ids.contains(&"p2".to_string()));
    }

    // ── No references returns empty ───────────────────────────────────

    #[test]
    fn no_references_returns_empty() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();
        insert_doc(&conn, "media", "m1");

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert!(refs.is_empty());
    }

    // ── Has-many relationship ─────────────────────────────────────────

    #[test]
    fn has_many_finds_back_reference() {
        let tags = CollectionDefinition::new("tags");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[tags, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "tags", "t1");
        insert_doc(&conn, "posts", "p1");
        insert_doc(&conn, "posts", "p2");
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES (?1, ?2, 0)",
            ["p1", "t1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES (?1, ?2, 0)",
            ["p2", "t1"],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "tags", "t1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].count, 2);
    }

    // ── Polymorphic has-one ───────────────────────────────────────────

    #[test]
    fn polymorphic_has_one_finds_back_reference() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("featured", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: "media".to_string(),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec!["media".to_string(), "pages".to_string()],
                })
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, pages, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "featured", "media/m1");

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].count, 1);
    }

    // ── Polymorphic has-many ──────────────────────────────────────────

    #[test]
    fn polymorphic_has_many_finds_back_reference() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: "media".to_string(),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec!["media".to_string(), "pages".to_string()],
                })
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, pages, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES (?1, ?2, ?3, 0)",
            ["p1", "m1", "media"],
        ).unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].count, 1);
    }

    // ── Group nesting ─────────────────────────────────────────────────

    #[test]
    fn group_nested_relationship_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("hero", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "meta__hero", "m1");

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].field_name, "hero");
    }

    // ── Array sub-field relationship ──────────────────────────────────

    #[test]
    fn array_sub_field_relationship_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("image", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, image) VALUES ('s1', 'p1', 0, 'm1')",
            [],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].field_name, "slides.image");
        assert_eq!(refs[0].count, 1);
    }

    // ── Blocks sub-field relationship ─────────────────────────────────

    #[test]
    fn blocks_sub_field_relationship_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![
                        FieldDefinition::builder("bg_image", FieldType::Upload)
                            .relationship(RelationshipConfig::new("media", false))
                            .build(),
                    ],
                )])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");
        conn.execute(
            "INSERT INTO posts_content (id, parent_id, _order, _block_type, data) VALUES ('b1', 'p1', 0, 'hero', '{\"bg_image\":\"m1\"}')",
            [],
        ).unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].field_name, "content.hero.bg_image");
        assert_eq!(refs[0].count, 1);
    }

    // ── Global back-reference ─────────────────────────────────────────

    #[test]
    fn global_back_reference_found() {
        let media = CollectionDefinition::new("media");
        let mut settings = GlobalDefinition::new("settings");
        settings.fields = vec![
            FieldDefinition::builder("logo", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media], &[settings], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        // Globals auto-create a single row during migration. Update it.
        conn.execute("UPDATE _global_settings SET logo = ?1", ["m1"])
            .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "settings");
        assert!(refs[0].is_global);
    }

    // ── Localized has-one ─────────────────────────────────────────────

    #[test]
    fn localized_has_one_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("hero", FieldType::Upload)
                .localized(true)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let locale = locale_en_de();

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &locale);
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        conn.execute("INSERT INTO posts (id, hero__en) VALUES ('p1', 'm1')", [])
            .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &locale);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].count, 1);
    }

    // ── Multiple collections referencing same target ──────────────────

    #[test]
    fn multiple_collections_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        let mut pages = CollectionDefinition::new("pages");
        pages.fields = vec![
            FieldDefinition::builder("banner", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts, pages], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "image", "m1");
        insert_doc_with_field(&conn, "pages", "pg1", "banner", "m1");

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert_eq!(refs.len(), 2);
        let slugs: Vec<&str> = refs.iter().map(|r| r.owner_slug.as_str()).collect();
        assert!(slugs.contains(&"posts"));
        assert!(slugs.contains(&"pages"));
    }

    // ── Unrelated collection not included ─────────────────────────────

    #[test]
    fn unrelated_collection_not_included() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();
        insert_doc(&conn, "media", "m1");

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale());
        assert!(refs.is_empty());
    }

    // ── find_missing_relations tests ─────────────────────────────────

    #[test]
    fn missing_has_one_detected() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();
        insert_doc(&conn, "media", "m1");

        let snapshot = serde_json::json!({"title": "Hello", "image": "m_deleted"});
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "image");
        assert_eq!(missing[0].missing_count, 1);
        assert_eq!(missing[0].total_ids, 1);
        assert!(missing[0].missing_ids.contains(&"m_deleted".to_string()));
    }

    #[test]
    fn no_missing_returns_empty() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();
        insert_doc(&conn, "media", "m1");

        let snapshot = serde_json::json!({"image": "m1"});
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert!(missing.is_empty());
    }

    #[test]
    fn missing_has_many_detected() {
        let tags = CollectionDefinition::new("tags");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[tags, posts], &[], &no_locale());
        let conn = pool.get().unwrap();
        insert_doc(&conn, "tags", "t1");

        let snapshot = serde_json::json!({"tags": ["t1", "t2"]});
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "tags");
        assert_eq!(missing[0].missing_count, 1);
        assert_eq!(missing[0].total_ids, 2);
        assert!(missing[0].missing_ids.contains(&"t2".to_string()));
    }

    #[test]
    fn missing_polymorphic_has_one() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("featured", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: "media".to_string(),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec!["media".to_string(), "pages".to_string()],
                })
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[media, pages, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        let snapshot = serde_json::json!({"featured": "media/m1"});
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].missing_ids.contains(&"media/m1".to_string()));
    }

    #[test]
    fn missing_group_nested_relation() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("hero", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        let snapshot = serde_json::json!({"meta__hero": "m_gone"});
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "hero");
    }

    #[test]
    fn missing_array_sub_field_relation() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("image", FieldType::Upload)
                        .relationship(RelationshipConfig::new("media", false))
                        .build(),
                ])
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();
        insert_doc(&conn, "media", "m1");

        let snapshot = serde_json::json!({
            "slides": [
                {"image": "m1"},
                {"image": "m_deleted"}
            ]
        });
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "slides.image");
        assert_eq!(missing[0].missing_count, 1);
        assert_eq!(missing[0].total_ids, 2);
    }

    #[test]
    fn missing_blocks_sub_field_relation() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![
                        FieldDefinition::builder("bg_image", FieldType::Upload)
                            .relationship(RelationshipConfig::new("media", false))
                            .build(),
                    ],
                )])
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        let snapshot = serde_json::json!({
            "content": [
                {"_block_type": "hero", "bg_image": "m_gone"}
            ]
        });
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "content.hero.bg_image");
    }

    #[test]
    fn empty_snapshot_returns_empty() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
        ];
        posts.fields = fields.clone();

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        let snapshot = serde_json::json!({});
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert!(missing.is_empty());
    }
}
