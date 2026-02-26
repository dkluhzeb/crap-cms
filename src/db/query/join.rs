//! Join table operations: has-many relationships, arrays, blocks, hydration.

use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::core::{CollectionDefinition, Document};
use crate::core::field::{FieldDefinition, FieldType};
use super::{coerce_value, LocaleContext, LocaleMode};

/// Resolve the effective locale string for a join table operation.
/// Returns Some("en") when the field is localized and locale is enabled,
/// None otherwise (same pattern as locale_write_column for regular columns).
fn resolve_join_locale(
    field: &FieldDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Option<String> {
    let ctx = locale_ctx?;
    if !field.localized || !ctx.config.is_enabled() { return None; }
    let locale = match &ctx.mode {
        LocaleMode::Single(l) => l.as_str(),
        _ => ctx.config.default_locale.as_str(),
    };
    Some(locale.to_string())
}

/// When fallback is enabled and we're querying a non-default locale,
/// returns the default locale to fall back to if the primary query returns empty.
fn resolve_join_fallback_locale(
    field: &FieldDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Option<String> {
    let ctx = locale_ctx?;
    if !field.localized || !ctx.config.is_enabled() || !ctx.config.fallback { return None; }
    match &ctx.mode {
        LocaleMode::Single(l) if l != &ctx.config.default_locale => {
            Some(ctx.config.default_locale.clone())
        }
        _ => None,
    }
}

/// Set related IDs for a has-many relationship junction table.
/// Deletes all existing rows for the parent and inserts new ones with _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_related_ids(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    ids: &[String],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2", table_name),
            rusqlite::params![parent_id, loc],
        ).with_context(|| format!("Failed to clear junction table {}", table_name))?;
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        ).with_context(|| format!("Failed to clear junction table {}", table_name))?;
    }

    if let Some(loc) = locale {
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, _order, _locale) VALUES (?1, ?2, ?3, ?4)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, id) in ids.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, id, i as i64, loc])?;
        }
    } else {
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, _order) VALUES (?1, ?2, ?3)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, id) in ids.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, id, i as i64])?;
        }
    }
    Ok(())
}

/// Find related IDs for a has-many relationship junction table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_related_ids(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<String>> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        let sql = format!(
            "SELECT related_id FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids: Vec<String> = stmt.query_map(rusqlite::params![parent_id, loc], |row| {
            row.get::<_, String>(0)
        })?.filter_map(|r| r.ok()).collect();
        Ok(ids)
    } else {
        let sql = format!(
            "SELECT related_id FROM {} WHERE parent_id = ?1 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids: Vec<String> = stmt.query_map([parent_id], |row| {
            row.get::<_, String>(0)
        })?.filter_map(|r| r.ok()).collect();
        Ok(ids)
    }
}

/// Set array rows for an array field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_array_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[HashMap<String, String>],
    sub_fields: &[crate::core::field::FieldDefinition],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field_name);
    if let Some(loc) = locale {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2", table_name),
            rusqlite::params![parent_id, loc],
        ).with_context(|| format!("Failed to clear array table {}", table_name))?;
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        ).with_context(|| format!("Failed to clear array table {}", table_name))?;
    }

    if rows.is_empty() || sub_fields.is_empty() {
        return Ok(());
    }

    // Build column list from sub-fields
    let col_names: Vec<&str> = sub_fields.iter().map(|f| f.name.as_str()).collect();
    let (all_cols, placeholders) = if let Some(_) = locale {
        let all_cols = format!(
            "id, parent_id, _order, _locale, {}",
            col_names.join(", ")
        );
        let placeholders = format!(
            "?1, ?2, ?3, ?4, {}",
            (5..5 + col_names.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(", ")
        );
        (all_cols, placeholders)
    } else {
        let all_cols = format!(
            "id, parent_id, _order, {}",
            col_names.join(", ")
        );
        let placeholders = format!(
            "?1, ?2, ?3, {}",
            (4..4 + col_names.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(", ")
        );
        (all_cols, placeholders)
    };
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", table_name, all_cols, placeholders);

    let mut stmt = conn.prepare(&sql)?;
    for (order, row) in rows.iter().enumerate() {
        let id = nanoid::nanoid!();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(id),
            Box::new(parent_id.to_string()),
            Box::new(order as i64),
        ];
        if let Some(loc) = locale {
            params.push(Box::new(loc.to_string()));
        }
        for sf in sub_fields {
            let value = row.get(&sf.name).cloned().unwrap_or_default();
            params.push(coerce_value(&sf.field_type, &value));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        stmt.execute(rusqlite::params_from_iter(param_refs.iter()))?;
    }
    Ok(())
}

/// Find array rows for an array field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_array_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    sub_fields: &[crate::core::field::FieldDefinition],
    locale: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let table_name = format!("{}_{}", collection, field_name);
    let col_names: Vec<&str> = sub_fields.iter().map(|f| f.name.as_str()).collect();
    let select_cols = if col_names.is_empty() {
        "id".to_string()
    } else {
        format!("id, {}", col_names.join(", "))
    };
    let sql = if locale.is_some() {
        format!(
            "SELECT {} FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            select_cols, table_name
        )
    } else {
        format!(
            "SELECT {} FROM {} WHERE parent_id = ?1 ORDER BY _order",
            select_cols, table_name
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(loc) = locale {
        vec![Box::new(parent_id.to_string()), Box::new(loc.to_string())]
    } else {
        vec![Box::new(parent_id.to_string())]
    };
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(param_refs.iter()), |row| {
        let mut map = serde_json::Map::new();
        let id: String = row.get(0)?;
        map.insert("id".to_string(), serde_json::Value::String(id));
        for (i, sf) in sub_fields.iter().enumerate() {
            let val: rusqlite::types::Value = row.get(i + 1)?;
            let json_val = match val {
                rusqlite::types::Value::Null => serde_json::Value::Null,
                rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                rusqlite::types::Value::Text(s) => {
                    // Composite sub-fields store JSON in TEXT columns —
                    // attempt to parse so nested data comes back structured.
                    match sf.field_type {
                        FieldType::Array | FieldType::Blocks | FieldType::Group | FieldType::Json => {
                            serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
                        }
                        _ => serde_json::Value::String(s),
                    }
                }
                rusqlite::types::Value::Blob(_) => serde_json::Value::Null,
            };
            map.insert(sf.name.clone(), json_val);
        }
        Ok(serde_json::Value::Object(map))
    })?.filter_map(|r| r.ok()).collect();
    Ok(rows)
}

/// Set block rows for a blocks field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
/// When `locale` is Some, scopes the DELETE to that locale and includes `_locale` in INSERT.
pub fn set_block_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[serde_json::Value],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field_name);
    if let Some(loc) = locale {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2", table_name),
            rusqlite::params![parent_id, loc],
        ).with_context(|| format!("Failed to clear blocks table {}", table_name))?;
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        ).with_context(|| format!("Failed to clear blocks table {}", table_name))?;
    }

    if rows.is_empty() {
        return Ok(());
    }

    if let Some(loc) = locale {
        let sql = format!(
            "INSERT INTO {} (id, parent_id, _order, _block_type, data, _locale) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (order, row) in rows.iter().enumerate() {
            let id = nanoid::nanoid!();
            let block_type = row.get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let mut data_map = match row.as_object() {
                Some(m) => m.clone(),
                None => serde_json::Map::new(),
            };
            data_map.remove("_block_type");
            data_map.remove("id");
            let data_json = serde_json::Value::Object(data_map).to_string();
            stmt.execute(rusqlite::params![id, parent_id, order as i64, block_type, data_json, loc])?;
        }
    } else {
        let sql = format!(
            "INSERT INTO {} (id, parent_id, _order, _block_type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (order, row) in rows.iter().enumerate() {
            let id = nanoid::nanoid!();
            let block_type = row.get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let mut data_map = match row.as_object() {
                Some(m) => m.clone(),
                None => serde_json::Map::new(),
            };
            data_map.remove("_block_type");
            data_map.remove("id");
            let data_json = serde_json::Value::Object(data_map).to_string();
            stmt.execute(rusqlite::params![id, parent_id, order as i64, block_type, data_json])?;
        }
    }
    Ok(())
}

/// Find block rows for a blocks field join table, ordered.
/// When `locale` is Some, filters by `_locale`.
pub fn find_block_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<serde_json::Value>> {
    let table_name = format!("{}_{}", collection, field_name);
    let sql = if locale.is_some() {
        format!(
            "SELECT id, _block_type, data FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            table_name
        )
    } else {
        format!(
            "SELECT id, _block_type, data FROM {} WHERE parent_id = ?1 ORDER BY _order",
            table_name
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(loc) = locale {
        vec![Box::new(parent_id.to_string()), Box::new(loc.to_string())]
    } else {
        vec![Box::new(parent_id.to_string())]
    };
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(param_refs.iter()), |row| {
        let id: String = row.get(0)?;
        let block_type: String = row.get(1)?;
        let data_json: String = row.get(2)?;
        Ok((id, block_type, data_json))
    })?.filter_map(|r| r.ok()).map(|(id, block_type, data_json)| {
        let mut map = match serde_json::from_str::<serde_json::Value>(&data_json) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        map.insert("id".to_string(), serde_json::Value::String(id));
        map.insert("_block_type".to_string(), serde_json::Value::String(block_type));
        serde_json::Value::Object(map)
    }).collect();
    Ok(rows)
}

/// Hydrate a document with join table data (has-many relationships and arrays).
/// Populates `doc.fields` with JSON arrays for each join-table field.
/// If `select` is provided, skip hydrating fields not in the select list.
/// When `locale_ctx` is provided, localized join fields are filtered by locale.
pub fn hydrate_document(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in &def.fields {
        // Skip hydrating fields not in the select list
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let locale = resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        let fallback_locale = resolve_join_fallback_locale(field, locale_ctx);
        let fallback_ref = fallback_locale.as_deref();
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        let mut ids = find_related_ids(conn, slug, &field.name, &doc.id, locale_ref)?;
                        if ids.is_empty() && fallback_ref.is_some() {
                            ids = find_related_ids(conn, slug, &field.name, &doc.id, fallback_ref)?;
                        }
                        let json_ids: Vec<serde_json::Value> = ids.into_iter()
                            .map(serde_json::Value::String)
                            .collect();
                        doc.fields.insert(field.name.clone(), serde_json::Value::Array(json_ids));
                    }
                }
            }
            FieldType::Array => {
                let mut rows = find_array_rows(conn, slug, &field.name, &doc.id, &field.fields, locale_ref)?;
                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_array_rows(conn, slug, &field.name, &doc.id, &field.fields, fallback_ref)?;
                }
                doc.fields.insert(field.name.clone(), serde_json::Value::Array(rows));
            }
            FieldType::Group => {
                // Reconstruct nested object from prefixed columns: seo__title → { seo: { title: val } }
                let mut group_obj = serde_json::Map::new();
                for sub in &field.fields {
                    let col_name = format!("{}__{}", field.name, sub.name);
                    if let Some(val) = doc.fields.remove(&col_name) {
                        group_obj.insert(sub.name.clone(), val);
                    }
                }
                doc.fields.insert(field.name.clone(), serde_json::Value::Object(group_obj));
            }
            FieldType::Blocks => {
                let mut rows = find_block_rows(conn, slug, &field.name, &doc.id, locale_ref)?;
                if rows.is_empty() && fallback_ref.is_some() {
                    rows = find_block_rows(conn, slug, &field.name, &doc.id, fallback_ref)?;
                }
                doc.fields.insert(field.name.clone(), serde_json::Value::Array(rows));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Save join table data for has-many relationships and arrays.
/// Extracts relevant data from the data map and writes to join tables.
/// When `locale_ctx` is provided, localized join fields are scoped by locale.
pub fn save_join_table_data(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    parent_id: &str,
    data: &HashMap<String, serde_json::Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in &def.fields {
        let locale = resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        // Only touch join table if the field was explicitly included in the data.
                        if let Some(val) = data.get(&field.name) {
                            let ids = match val {
                                serde_json::Value::Array(arr) => {
                                    arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                                }
                                serde_json::Value::String(s) => {
                                    if s.is_empty() {
                                        Vec::new()
                                    } else {
                                        s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                                    }
                                }
                                _ => Vec::new(),
                            };
                            set_related_ids(conn, slug, &field.name, parent_id, &ids, locale_ref)?;
                        }
                    }
                }
            }
            FieldType::Array => {
                if let Some(val) = data.get(&field.name) {
                    let rows = match val {
                        serde_json::Value::Array(arr) => {
                            arr.iter().filter_map(|v| {
                                if let serde_json::Value::Object(map) = v {
                                    let row: HashMap<String, String> = map.iter().map(|(k, v)| {
                                        let s = match v {
                                            serde_json::Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        };
                                        (k.clone(), s)
                                    }).collect();
                                    Some(row)
                                } else {
                                    None
                                }
                            }).collect()
                        }
                        _ => Vec::new(),
                    };
                    set_array_rows(conn, slug, &field.name, parent_id, &rows, &field.fields, locale_ref)?;
                }
            }
            FieldType::Blocks => {
                if let Some(val) = data.get(&field.name) {
                    let rows = match val {
                        serde_json::Value::Array(arr) => arr.clone(),
                        _ => Vec::new(),
                    };
                    set_block_rows(conn, slug, &field.name, parent_id, &rows, locale_ref)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::core::collection::*;
    use crate::core::field::*;

    fn setup_join_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            -- Has-many junction table
            CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER
            );
            -- Array join table
            CREATE TABLE posts_items (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                label TEXT,
                value TEXT
            );
            -- Blocks join table
            CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT
            );
            INSERT INTO posts (id, title, created_at, updated_at) VALUES ('p1', 'Post 1', '2024-01-01', '2024-01-01');"
        ).unwrap();
        conn
    }

    fn array_sub_fields() -> Vec<FieldDefinition> {
        vec![
            FieldDefinition {
                name: "label".to_string(),
                field_type: FieldType::Text,
                required: false, unique: false, validate: None, default_value: None,
                options: vec![], admin: FieldAdmin::default(), hooks: FieldHooks::default(),
                access: FieldAccess::default(), relationship: None, fields: vec![],
                blocks: vec![], localized: false, picker_appearance: None,
            },
            FieldDefinition {
                name: "value".to_string(),
                field_type: FieldType::Text,
                required: false, unique: false, validate: None, default_value: None,
                options: vec![], admin: FieldAdmin::default(), hooks: FieldHooks::default(),
                access: FieldAccess::default(), relationship: None, fields: vec![],
                blocks: vec![], localized: false, picker_appearance: None,
            },
        ]
    }

    fn posts_def_with_joins() -> CollectionDefinition {
        CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "title".to_string(),
                    field_type: FieldType::Text,
                    required: false, unique: false, validate: None, default_value: None,
                    options: vec![], admin: FieldAdmin::default(), hooks: FieldHooks::default(),
                    access: FieldAccess::default(), relationship: None, fields: vec![],
                    blocks: vec![], localized: false, picker_appearance: None,
                },
                FieldDefinition {
                    name: "tags".to_string(),
                    field_type: FieldType::Relationship,
                    relationship: Some(RelationshipConfig {
                        collection: "tags".to_string(),
                        has_many: true,
                        max_depth: None,
                    }),
                    required: false, unique: false, validate: None, default_value: None,
                    options: vec![], admin: FieldAdmin::default(), hooks: FieldHooks::default(),
                    access: FieldAccess::default(), fields: vec![],
                    blocks: vec![], localized: false, picker_appearance: None,
                },
                FieldDefinition {
                    name: "items".to_string(),
                    field_type: FieldType::Array,
                    fields: array_sub_fields(),
                    required: false, unique: false, validate: None, default_value: None,
                    options: vec![], admin: FieldAdmin::default(), hooks: FieldHooks::default(),
                    access: FieldAccess::default(), relationship: None,
                    blocks: vec![], localized: false, picker_appearance: None,
                },
                FieldDefinition {
                    name: "content".to_string(),
                    field_type: FieldType::Blocks,
                    required: false, unique: false, validate: None, default_value: None,
                    options: vec![], admin: FieldAdmin::default(), hooks: FieldHooks::default(),
                    access: FieldAccess::default(), relationship: None, fields: vec![],
                    blocks: vec![], localized: false, picker_appearance: None,
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
        versions: None,
        }
    }

    // ── set_related_ids + find_related_ids ───────────────────────────────────

    #[test]
    fn set_and_find_related_ids() {
        let conn = setup_join_db();
        let ids = vec!["t1".to_string(), "t2".to_string(), "t3".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2", "t3"], "Should return IDs in insertion order");
    }

    #[test]
    fn replace_related_ids() {
        let conn = setup_join_db();
        let ids_old = vec!["t1".to_string(), "t2".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids_old, None).unwrap();

        let ids_new = vec!["t3".to_string(), "t4".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids_new, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t3", "t4"], "Old IDs should be replaced by new ones");
    }

    #[test]
    fn empty_related_ids() {
        let conn = setup_join_db();
        // Set some IDs first, then clear them
        let ids = vec!["t1".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &ids, None).unwrap();
        set_related_ids(&conn, "posts", "tags", "p1", &[], None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(found.is_empty(), "Should return empty list after setting empty IDs");
    }

    // ── set_array_rows + find_array_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_array_rows() {
        let conn = setup_join_db();
        let sub = array_sub_fields();
        let rows = vec![
            HashMap::from([
                ("label".to_string(), "Label A".to_string()),
                ("value".to_string(), "Value A".to_string()),
            ]),
            HashMap::from([
                ("label".to_string(), "Label B".to_string()),
                ("value".to_string(), "Value B".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["label"], "Label A");
        assert_eq!(found[0]["value"], "Value A");
        assert_eq!(found[1]["label"], "Label B");
        assert_eq!(found[1]["value"], "Value B");
        // Each row should have an id
        assert!(found[0]["id"].as_str().is_some(), "Row should have an id");
        assert!(found[1]["id"].as_str().is_some(), "Row should have an id");
    }

    #[test]
    fn replace_array_rows() {
        let conn = setup_join_db();
        let sub = array_sub_fields();
        let rows_old = vec![
            HashMap::from([
                ("label".to_string(), "Old".to_string()),
                ("value".to_string(), "Old Val".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows_old, &sub, None).unwrap();

        let rows_new = vec![
            HashMap::from([
                ("label".to_string(), "New".to_string()),
                ("value".to_string(), "New Val".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows_new, &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert_eq!(found.len(), 1, "Old rows should be replaced");
        assert_eq!(found[0]["label"], "New");
        assert_eq!(found[0]["value"], "New Val");
    }

    #[test]
    fn empty_array_rows() {
        let conn = setup_join_db();
        let sub = array_sub_fields();
        let rows = vec![
            HashMap::from([
                ("label".to_string(), "X".to_string()),
                ("value".to_string(), "Y".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();
        set_array_rows(&conn, "posts", "items", "p1", &[], &sub, None).unwrap();

        let found = find_array_rows(&conn, "posts", "items", "p1", &sub, None).unwrap();
        assert!(found.is_empty(), "Should return empty after setting empty rows");
    }

    // ── set_block_rows + find_block_rows ─────────────────────────────────────

    #[test]
    fn set_and_find_block_rows() {
        let conn = setup_join_db();
        let blocks = vec![
            serde_json::json!({"_block_type": "paragraph", "text": "Hello world"}),
            serde_json::json!({"_block_type": "image", "url": "/img/photo.jpg", "alt": "A photo"}),
        ];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["_block_type"], "paragraph");
        assert_eq!(found[0]["text"], "Hello world");
        assert_eq!(found[1]["_block_type"], "image");
        assert_eq!(found[1]["url"], "/img/photo.jpg");
        assert_eq!(found[1]["alt"], "A photo");
        // Each block should have an id
        assert!(found[0]["id"].as_str().is_some(), "Block should have an id");
        assert!(found[1]["id"].as_str().is_some(), "Block should have an id");
    }

    #[test]
    fn replace_block_rows() {
        let conn = setup_join_db();
        let blocks_old = vec![
            serde_json::json!({"_block_type": "paragraph", "text": "Old text"}),
        ];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_old, None).unwrap();

        let blocks_new = vec![
            serde_json::json!({"_block_type": "heading", "level": 1, "text": "New heading"}),
        ];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_new, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 1, "Old blocks should be replaced");
        assert_eq!(found[0]["_block_type"], "heading");
        assert_eq!(found[0]["text"], "New heading");
    }

    // ── hydrate_document ─────────────────────────────────────────────────────

    #[test]
    fn hydrate_has_many_and_array() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        // Set up has-many relationship data
        let tag_ids = vec!["t1".to_string(), "t2".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &tag_ids, None).unwrap();

        // Set up array data
        let sub = array_sub_fields();
        let rows = vec![
            HashMap::from([
                ("label".to_string(), "Item 1".to_string()),
                ("value".to_string(), "Val 1".to_string()),
            ]),
        ];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        // Set up blocks data
        let blocks = vec![
            serde_json::json!({"_block_type": "text", "body": "Hello"}),
        ];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, None).unwrap();

        // Create a document to hydrate
        let mut doc = crate::core::Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Post 1"));

        hydrate_document(&conn, "posts", &def, &mut doc, None, None).unwrap();

        // Verify has-many tags
        let tags = doc.fields.get("tags").expect("tags should be populated");
        let tags_arr = tags.as_array().expect("tags should be an array");
        assert_eq!(tags_arr.len(), 2);
        assert_eq!(tags_arr[0], "t1");
        assert_eq!(tags_arr[1], "t2");

        // Verify array items
        let items = doc.fields.get("items").expect("items should be populated");
        let items_arr = items.as_array().expect("items should be an array");
        assert_eq!(items_arr.len(), 1);
        assert_eq!(items_arr[0]["label"], "Item 1");
        assert_eq!(items_arr[0]["value"], "Val 1");

        // Verify blocks content
        let content = doc.fields.get("content").expect("content should be populated");
        let content_arr = content.as_array().expect("content should be an array");
        assert_eq!(content_arr.len(), 1);
        assert_eq!(content_arr[0]["_block_type"], "text");
        assert_eq!(content_arr[0]["body"], "Hello");

        // Original title field should be unchanged
        assert_eq!(doc.get_str("title"), Some("Post 1"));
    }
}
