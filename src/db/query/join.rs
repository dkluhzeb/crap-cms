//! Join table operations: has-many relationships, arrays, blocks, hydration.

use anyhow::{Context as _, Result};
use std::collections::HashMap;

use crate::core::Document;
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

/// Set related items for a polymorphic has-many relationship junction table.
/// Each item is a `(related_collection, related_id)` pair.
/// Deletes all existing rows for the parent and inserts new ones with _order.
pub fn set_polymorphic_related(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    items: &[(String, String)],
    locale: Option<&str>,
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1 AND _locale = ?2", table_name),
            rusqlite::params![parent_id, loc],
        )?;
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, related_collection, _order, _locale) VALUES (?1, ?2, ?3, ?4, ?5)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, rel_id, rel_col, i as i64, loc])?;
        }
    } else {
        conn.execute(
            &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
            [parent_id],
        )?;
        let sql = format!(
            "INSERT INTO {} (parent_id, related_id, related_collection, _order) VALUES (?1, ?2, ?3, ?4)",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        for (i, (rel_col, rel_id)) in items.iter().enumerate() {
            stmt.execute(rusqlite::params![parent_id, rel_id, rel_col, i as i64])?;
        }
    }
    Ok(())
}

/// Find related items for a polymorphic has-many relationship junction table.
/// Returns `(related_collection, related_id)` pairs ordered by _order.
pub fn find_polymorphic_related(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    locale: Option<&str>,
) -> Result<Vec<(String, String)>> {
    let table_name = format!("{}_{}", collection, field);
    if let Some(loc) = locale {
        let sql = format!(
            "SELECT related_collection, related_id FROM {} WHERE parent_id = ?1 AND _locale = ?2 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let items: Vec<(String, String)> = stmt.query_map(rusqlite::params![parent_id, loc], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?.filter_map(|r| r.ok()).collect();
        Ok(items)
    } else {
        let sql = format!(
            "SELECT related_collection, related_id FROM {} WHERE parent_id = ?1 ORDER BY _order",
            table_name
        );
        let mut stmt = conn.prepare(&sql)?;
        let items: Vec<(String, String)> = stmt.query_map([parent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?.filter_map(|r| r.ok()).collect();
        Ok(items)
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

    let flat_subs = crate::core::field::flatten_array_sub_fields(sub_fields);

    if rows.is_empty() || flat_subs.is_empty() {
        return Ok(());
    }

    // Build column list from flattened sub-fields
    let col_names: Vec<&str> = flat_subs.iter().map(|f| f.name.as_str()).collect();
    let (all_cols, placeholders) = if locale.is_some() {
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
        for sf in &flat_subs {
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
    let flat_subs = crate::core::field::flatten_array_sub_fields(sub_fields);
    let col_names: Vec<&str> = flat_subs.iter().map(|f| f.name.as_str()).collect();
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
        for (i, sf) in flat_subs.iter().enumerate() {
            let val: rusqlite::types::Value = row.get(i + 1)?;
            let json_val = match val {
                rusqlite::types::Value::Null => serde_json::Value::Null,
                rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                rusqlite::types::Value::Text(s) => {
                    // Composite sub-fields store JSON in TEXT columns —
                    // attempt to parse so nested data comes back structured.
                    match sf.field_type {
                        FieldType::Array | FieldType::Blocks | FieldType::Group | FieldType::Row | FieldType::Collapsible | FieldType::Tabs | FieldType::Json => {
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

/// Recursively extract prefixed columns from `doc.fields` into a nested Group object.
/// Handles Group→Row, Group→Collapsible, Group→Tabs, and Group→Group nesting.
fn reconstruct_group_fields(
    fields: &[FieldDefinition],
    prefix: &str,
    doc: &mut Document,
    group_obj: &mut serde_json::Map<String, serde_json::Value>,
) {
    for sub in fields {
        match sub.field_type {
            FieldType::Group => {
                // Nested group: collect sub-group's fields into a nested object
                let new_prefix = format!("{}__{}", prefix, sub.name);
                let mut sub_obj = serde_json::Map::new();
                reconstruct_group_fields(&sub.fields, &new_prefix, doc, &mut sub_obj);
                if !sub_obj.is_empty() {
                    group_obj.insert(sub.name.clone(), serde_json::Value::Object(sub_obj));
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                // Layout fields are transparent — promote sub-fields to same level
                reconstruct_group_fields(&sub.fields, prefix, doc, group_obj);
            }
            FieldType::Tabs => {
                for tab in &sub.tabs {
                    reconstruct_group_fields(&tab.fields, prefix, doc, group_obj);
                }
            }
            _ => {
                let col_name = format!("{}__{}", prefix, sub.name);
                if let Some(val) = doc.fields.remove(&col_name) {
                    group_obj.insert(sub.name.clone(), val);
                }
            }
        }
    }
}

/// Parse polymorphic relationship values from form data.
/// Accepts "collection/id" composite strings from either a JSON array or comma-separated string.
fn parse_polymorphic_values(val: &serde_json::Value) -> Vec<(String, String)> {
    let raw_items: Vec<String> = match val {
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
    raw_items.into_iter().filter_map(|item| {
        // Parse "collection/id" format
        if let Some(pos) = item.find('/') {
            let col = item[..pos].to_string();
            let id = item[pos + 1..].to_string();
            if !col.is_empty() && !id.is_empty() {
                return Some((col, id));
            }
        }
        None
    }).collect()
}

/// Hydrate a document with join table data (has-many relationships and arrays).
/// Populates `doc.fields` with JSON arrays for each join-table field.
/// If `select` is provided, skip hydrating fields not in the select list.
/// When `locale_ctx` is provided, localized join fields are filtered by locale.
pub fn hydrate_document(
    conn: &rusqlite::Connection,
    slug: &str,
    fields: &[FieldDefinition],
    doc: &mut Document,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in fields {
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
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        if rc.is_polymorphic() {
                            let mut items = find_polymorphic_related(conn, slug, &field.name, &doc.id, locale_ref)?;
                            if items.is_empty() && fallback_ref.is_some() {
                                items = find_polymorphic_related(conn, slug, &field.name, &doc.id, fallback_ref)?;
                            }
                            let json_items: Vec<serde_json::Value> = items.into_iter()
                                .map(|(col, id)| serde_json::Value::String(format!("{}/{}", col, id)))
                                .collect();
                            doc.fields.insert(field.name.clone(), serde_json::Value::Array(json_items));
                        } else {
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
                let prefix = &field.name;
                reconstruct_group_fields(&field.fields, prefix, doc, &mut group_obj);
                if !group_obj.is_empty() {
                    doc.fields.insert(field.name.clone(), serde_json::Value::Object(group_obj));
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                // Sub-fields are top-level columns, but recurse for join-table types (blocks, arrays, relationships)
                hydrate_document(conn, slug, &field.fields, doc, select, locale_ctx)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    hydrate_document(conn, slug, &tab.fields, doc, select, locale_ctx)?;
                }
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
    fields: &[FieldDefinition],
    parent_id: &str,
    data: &HashMap<String, serde_json::Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    for field in fields {
        let locale = resolve_join_locale(field, locale_ctx);
        let locale_ref = locale.as_deref();
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        // Only touch join table if the field was explicitly included in the data.
                        if let Some(val) = data.get(&field.name) {
                            if rc.is_polymorphic() {
                                // Polymorphic: values are "collection/id" composite strings
                                let items = parse_polymorphic_values(val);
                                set_polymorphic_related(conn, slug, &field.name, parent_id, &items, locale_ref)?;
                            } else {
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
            FieldType::Row | FieldType::Collapsible => {
                save_join_table_data(conn, slug, &field.fields, parent_id, data, locale_ctx)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    save_join_table_data(conn, slug, &tab.fields, parent_id, data, locale_ctx)?;
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
                ..Default::default()
            },
            FieldDefinition {
                name: "value".to_string(),
                ..Default::default()
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
                    ..Default::default()
                },
                FieldDefinition {
                    name: "tags".to_string(),
                    field_type: FieldType::Relationship,
                    relationship: Some(RelationshipConfig {
                        collection: "tags".to_string(),
                        has_many: true,
                        max_depth: None,
                        polymorphic: vec![],
                    }),
                    ..Default::default()
                },
                FieldDefinition {
                    name: "items".to_string(),
                    field_type: FieldType::Array,
                    fields: array_sub_fields(),
                    ..Default::default()
                },
                FieldDefinition {
                    name: "content".to_string(),
                    field_type: FieldType::Blocks,
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
        versions: None,
            indexes: Vec::new(),
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

        hydrate_document(&conn, "posts", &def.fields, &mut doc, None, None).unwrap();

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

    // ── hydrate_document with select ────────────────────────────────────────

    #[test]
    fn hydrate_with_select_filters_fields() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        // Set up data for all join types
        let tag_ids = vec!["t1".to_string()];
        set_related_ids(&conn, "posts", "tags", "p1", &tag_ids, None).unwrap();
        let sub = array_sub_fields();
        let rows = vec![HashMap::from([
            ("label".to_string(), "Item 1".to_string()),
            ("value".to_string(), "Val 1".to_string()),
        ])];
        set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

        let mut doc = crate::core::Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Post 1"));

        // Only hydrate "tags", skip "items" and "content"
        let select = vec!["tags".to_string(), "title".to_string()];
        hydrate_document(&conn, "posts", &def.fields, &mut doc, Some(&select), None).unwrap();

        assert!(doc.fields.contains_key("tags"), "tags should be hydrated");
        assert!(!doc.fields.contains_key("items"), "items should NOT be hydrated (not in select)");
        assert!(!doc.fields.contains_key("content"), "content should NOT be hydrated (not in select)");
    }

    // ── save_join_table_data ────────────────────────────────────────────────

    #[test]
    fn save_join_table_data_has_many_from_string() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        // Pass has-many IDs as a comma-separated string
        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!("t1, t2, t3"));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn save_join_table_data_has_many_from_array() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!(["t1", "t2"]));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1", "t2"]);
    }

    #[test]
    fn save_join_table_data_has_many_empty_string() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        // Pre-populate some IDs
        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string()], None).unwrap();

        // Sending an empty string should clear the junction table
        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!(""));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn save_join_table_data_blocks() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        let mut data = HashMap::new();
        data.insert("content".to_string(), serde_json::json!([
            {"_block_type": "paragraph", "text": "Hello"},
            {"_block_type": "image", "url": "/img.jpg"},
        ]));

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0]["_block_type"], "paragraph");
        assert_eq!(found[1]["_block_type"], "image");
    }

    #[test]
    fn save_join_table_data_skips_absent_fields() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        // Pre-populate tags
        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string()], None).unwrap();

        // Save data that does NOT include "tags" -- should NOT touch the junction table
        let data = HashMap::new(); // empty

        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert_eq!(found, vec!["t1"], "tags should be preserved when not in data");
    }

    // ── locale-aware join operations ────────────────────────────────────────

    #[test]
    fn set_and_find_related_ids_with_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_tags (
                parent_id TEXT,
                related_id TEXT,
                _order INTEGER,
                _locale TEXT
            );"
        ).unwrap();

        set_related_ids(&conn, "posts", "tags", "p1", &["t1".to_string(), "t2".to_string()], Some("en")).unwrap();
        set_related_ids(&conn, "posts", "tags", "p1", &["t3".to_string()], Some("de")).unwrap();

        let en = find_related_ids(&conn, "posts", "tags", "p1", Some("en")).unwrap();
        assert_eq!(en, vec!["t1", "t2"]);

        let de = find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap();
        assert_eq!(de, vec!["t3"]);

        // Replacing en should not affect de
        set_related_ids(&conn, "posts", "tags", "p1", &["t4".to_string()], Some("en")).unwrap();
        let en = find_related_ids(&conn, "posts", "tags", "p1", Some("en")).unwrap();
        assert_eq!(en, vec!["t4"]);
        let de = find_related_ids(&conn, "posts", "tags", "p1", Some("de")).unwrap();
        assert_eq!(de, vec!["t3"]);
    }

    // ── Group field hydration ───────────────────────────────────────────────

    #[test]
    fn hydrate_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                seo__meta_title TEXT,
                seo__meta_desc TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts VALUES ('p1', 'Test', 'SEO Title', 'SEO Desc', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let fields = vec![
            FieldDefinition {
                name: "title".to_string(),
                ..Default::default()
            },
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition { name: "meta_title".to_string(), ..Default::default() },
                    FieldDefinition { name: "meta_desc".to_string(), ..Default::default() },
                ],
                ..Default::default()
            },
        ];

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Test"));
        doc.fields.insert("seo__meta_title".to_string(), serde_json::json!("SEO Title"));
        doc.fields.insert("seo__meta_desc".to_string(), serde_json::json!("SEO Desc"));

        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        // Group fields should be reconstructed as nested objects
        let seo = doc.fields.get("seo").expect("seo group should exist");
        assert_eq!(seo.get("meta_title").and_then(|v| v.as_str()), Some("SEO Title"));
        assert_eq!(seo.get("meta_desc").and_then(|v| v.as_str()), Some("SEO Desc"));
        // Prefixed keys should be removed
        assert!(!doc.fields.contains_key("seo__meta_title"));
        assert!(!doc.fields.contains_key("seo__meta_desc"));
    }

    // ── Polymorphic relationship operations ─────────────────────────────────

    fn setup_polymorphic_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                author TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            -- Polymorphic has-many junction table with related_collection column
            CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                PRIMARY KEY (parent_id, related_id, related_collection)
            );
            INSERT INTO posts (id, title, created_at, updated_at) VALUES ('p1', 'Post 1', '2024-01-01', '2024-01-01');"
        ).unwrap();
        conn
    }

    #[test]
    fn set_and_find_polymorphic_related() {
        let conn = setup_polymorphic_db();
        let items = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
            ("articles".to_string(), "a2".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(found, vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
            ("articles".to_string(), "a2".to_string()),
        ]);
    }

    #[test]
    fn replace_polymorphic_related() {
        let conn = setup_polymorphic_db();
        let old = vec![("articles".to_string(), "a1".to_string())];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &old, None).unwrap();

        let new_items = vec![
            ("pages".to_string(), "pg1".to_string()),
            ("pages".to_string(), "pg2".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &new_items, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(found, vec![
            ("pages".to_string(), "pg1".to_string()),
            ("pages".to_string(), "pg2".to_string()),
        ]);
    }

    #[test]
    fn parse_polymorphic_values_from_json_array() {
        let val = serde_json::json!(["articles/a1", "pages/pg1"]);
        let items = parse_polymorphic_values(&val);
        assert_eq!(items, vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ]);
    }

    #[test]
    fn parse_polymorphic_values_from_comma_string() {
        let val = serde_json::json!("articles/a1,pages/pg1");
        let items = parse_polymorphic_values(&val);
        assert_eq!(items, vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ]);
    }

    #[test]
    fn parse_polymorphic_values_skips_invalid() {
        let val = serde_json::json!(["articles/a1", "no_slash", "", "pages/"]);
        let items = parse_polymorphic_values(&val);
        assert_eq!(items, vec![
            ("articles".to_string(), "a1".to_string()),
        ], "Should skip entries without valid collection/id format");
    }

    #[test]
    fn hydrate_polymorphic_has_many() {
        let conn = setup_polymorphic_db();
        let items = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items, None).unwrap();

        let fields = vec![
            FieldDefinition {
                name: "refs".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "articles".to_string(),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec!["articles".to_string(), "pages".to_string()],
                }),
                ..Default::default()
            },
        ];

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let refs = doc.fields.get("refs").expect("refs should be hydrated");
        let arr = refs.as_array().expect("should be array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str().unwrap(), "articles/a1");
        assert_eq!(arr[1].as_str().unwrap(), "pages/pg1");
    }

    #[test]
    fn save_join_table_data_polymorphic_has_many() {
        let conn = setup_polymorphic_db();
        let fields = vec![
            FieldDefinition {
                name: "refs".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "articles".to_string(),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec!["articles".to_string(), "pages".to_string()],
                }),
                ..Default::default()
            },
        ];

        let mut data = HashMap::new();
        data.insert("refs".to_string(), serde_json::json!("articles/a1,pages/pg1"));

        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let found = find_polymorphic_related(&conn, "posts", "refs", "p1", None).unwrap();
        assert_eq!(found, vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ]);
    }

    // ── Regression: blocks inside Tabs ──────────────────────────────────────

    #[test]
    fn save_and_hydrate_blocks_inside_tabs() {
        // Regression: blocks nested inside a Tabs field were lost on save and invisible on read
        let conn = setup_join_db();

        let blocks_field = FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            ..Default::default()
        };
        let tabs_field = FieldDefinition {
            name: "page_settings".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Content".to_string(),
                    description: None,
                    fields: vec![blocks_field],
                },
            ],
            ..Default::default()
        };
        let fields = vec![
            FieldDefinition { name: "title".to_string(), ..Default::default() },
            tabs_field,
        ];

        // Save blocks via the Tabs wrapper
        let mut data = HashMap::new();
        data.insert("content".to_string(), serde_json::json!([
            {"_block_type": "hero", "heading": "Welcome"},
            {"_block_type": "text", "body": "Hello world"},
        ]));
        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        // Verify blocks were written
        let rows = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(rows.len(), 2, "blocks should be saved through Tabs");
        assert_eq!(rows[0]["_block_type"], "hero");
        assert_eq!(rows[1]["_block_type"], "text");

        // Hydrate document and verify blocks come back
        let mut doc = Document {
            id: "p1".to_string(),
            fields: serde_json::Map::new().into_iter().collect(),
            created_at: None,
            updated_at: None,
        };
        doc.fields.insert("title".to_string(), serde_json::json!("Post 1"));
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let content = doc.fields.get("content").expect("blocks must be hydrated through Tabs");
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["_block_type"], "hero");
        assert_eq!(arr[1]["_block_type"], "text");
    }

    #[test]
    fn save_and_hydrate_array_inside_row() {
        // Regression: arrays nested inside a Row field were lost on save and invisible on read
        let conn = setup_join_db();

        let array_field = FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: array_sub_fields(),
            ..Default::default()
        };
        let row_field = FieldDefinition {
            name: "main_row".to_string(),
            field_type: FieldType::Row,
            fields: vec![array_field],
            ..Default::default()
        };
        let fields = vec![
            FieldDefinition { name: "title".to_string(), ..Default::default() },
            row_field,
        ];

        let mut data = HashMap::new();
        data.insert("items".to_string(), serde_json::json!([
            {"label": "First", "value": "1"},
            {"label": "Second", "value": "2"},
        ]));
        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let rows = find_array_rows(&conn, "posts", "items", "p1", &array_sub_fields(), None).unwrap();
        assert_eq!(rows.len(), 2, "array should be saved through Row");
        assert_eq!(rows[0]["label"], "First");
        assert_eq!(rows[1]["label"], "Second");

        let mut doc = Document {
            id: "p1".to_string(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        };
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let items = doc.fields.get("items").expect("array must be hydrated through Row");
        let arr = items.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["label"], "First");
        assert_eq!(arr[1]["value"], "2");
    }

    #[test]
    fn save_and_hydrate_blocks_inside_collapsible() {
        // Regression: blocks nested inside a Collapsible field were lost
        let conn = setup_join_db();

        let blocks_field = FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            ..Default::default()
        };
        let collapsible_field = FieldDefinition {
            name: "advanced".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![blocks_field],
            ..Default::default()
        };
        let fields = vec![
            FieldDefinition { name: "title".to_string(), ..Default::default() },
            collapsible_field,
        ];

        let mut data = HashMap::new();
        data.insert("content".to_string(), serde_json::json!([
            {"_block_type": "cta", "heading": "Buy now"},
        ]));
        save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

        let rows = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert_eq!(rows.len(), 1, "blocks should be saved through Collapsible");
        assert_eq!(rows[0]["_block_type"], "cta");

        let mut doc = Document {
            id: "p1".to_string(),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        };
        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        let content = doc.fields.get("content").expect("blocks must be hydrated through Collapsible");
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["_block_type"], "cta");
    }

    #[test]
    fn set_and_find_array_rows_with_tabs() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 title TEXT,
                 body TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');"
        ).unwrap();

        // Sub-fields wrapped in Tabs
        let sub_fields = vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab {
                        label: "General".to_string(),
                        description: None,
                        fields: vec![FieldDefinition {
                            name: "title".to_string(),
                            ..Default::default()
                        }],
                    },
                    FieldTab {
                        label: "Content".to_string(),
                        description: None,
                        fields: vec![FieldDefinition {
                            name: "body".to_string(),
                            ..Default::default()
                        }],
                    },
                ],
                ..Default::default()
            },
        ];

        let mut row = HashMap::new();
        row.insert("title".to_string(), "Hello".to_string());
        row.insert("body".to_string(), "World".to_string());
        set_array_rows(&conn, "posts", "items", "p1", &[row], &sub_fields, None).unwrap();

        let result = find_array_rows(&conn, "posts", "items", "p1", &sub_fields, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["title"], "Hello");
        assert_eq!(result[0]["body"], "World");
    }

    #[test]
    fn set_and_find_array_rows_with_row_wrapper() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 x TEXT,
                 y TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');"
        ).unwrap();

        let sub_fields = vec![
            FieldDefinition {
                name: "row_wrap".to_string(),
                field_type: FieldType::Row,
                fields: vec![
                    FieldDefinition { name: "x".to_string(), ..Default::default() },
                    FieldDefinition { name: "y".to_string(), ..Default::default() },
                ],
                ..Default::default()
            },
        ];

        let mut row = HashMap::new();
        row.insert("x".to_string(), "10".to_string());
        row.insert("y".to_string(), "20".to_string());
        set_array_rows(&conn, "posts", "items", "p1", &[row], &sub_fields, None).unwrap();

        let result = find_array_rows(&conn, "posts", "items", "p1", &sub_fields, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["x"], "10");
        assert_eq!(result[0]["y"], "20");
    }

    // ── find_array_rows with empty sub-fields ────────────────────────────

    #[test]
    fn find_array_rows_empty_sub_fields_returns_only_id() {
        // When there are no sub-fields, set_array_rows returns early (no rows inserted).
        // find_array_rows with empty sub_fields selects only "id" column.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_items (id, parent_id, _order) VALUES ('item1', 'p1', 0);"
        ).unwrap();

        // Sub-fields empty → selects only id
        let result = find_array_rows(&conn, "posts", "items", "p1", &[], None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["id"], "item1");
    }

    // ── parse_polymorphic_values: non-string, non-array input ────────────

    #[test]
    fn parse_polymorphic_values_from_null() {
        let val = serde_json::Value::Null;
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "null input should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_from_number() {
        let val = serde_json::json!(42);
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "number input should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_empty_string() {
        let val = serde_json::json!("");
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "empty string should yield no items");
    }

    #[test]
    fn parse_polymorphic_values_slash_prefix_only() {
        // "collection" present but id empty after slash
        let val = serde_json::json!(["articles/"]);
        let items = parse_polymorphic_values(&val);
        assert!(items.is_empty(), "/id empty should be skipped");
    }

    // ── polymorphic with locale ──────────────────────────────────────────

    #[test]
    fn set_and_find_polymorphic_related_with_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                _locale TEXT
            );"
        ).unwrap();

        let items_en = vec![
            ("articles".to_string(), "a1".to_string()),
            ("pages".to_string(), "pg1".to_string()),
        ];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items_en, Some("en")).unwrap();

        let items_de = vec![("articles".to_string(), "a2".to_string())];
        set_polymorphic_related(&conn, "posts", "refs", "p1", &items_de, Some("de")).unwrap();

        // en locale
        let en = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("en")).unwrap();
        assert_eq!(en.len(), 2);
        assert_eq!(en[0], ("articles".to_string(), "a1".to_string()));

        // de locale
        let de = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("de")).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0], ("articles".to_string(), "a2".to_string()));

        // Replacing en should not affect de
        set_polymorphic_related(&conn, "posts", "refs", "p1", &[("pages".to_string(), "pg2".to_string())], Some("en")).unwrap();
        let en2 = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("en")).unwrap();
        assert_eq!(en2.len(), 1);
        assert_eq!(en2[0], ("pages".to_string(), "pg2".to_string()));
        let de2 = find_polymorphic_related(&conn, "posts", "refs", "p1", Some("de")).unwrap();
        assert_eq!(de2.len(), 1, "de locale should be unchanged");
    }

    // ── set_block_rows with locale ───────────────────────────────────────

    #[test]
    fn set_and_find_block_rows_with_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );"
        ).unwrap();

        let blocks_en = vec![serde_json::json!({"_block_type": "text", "body": "Hello"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_en, Some("en")).unwrap();

        let blocks_de = vec![serde_json::json!({"_block_type": "text", "body": "Hallo"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks_de, Some("de")).unwrap();

        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert_eq!(en.len(), 1);
        assert_eq!(en[0]["body"], "Hello");

        let de = find_block_rows(&conn, "posts", "content", "p1", Some("de")).unwrap();
        assert_eq!(de.len(), 1);
        assert_eq!(de[0]["body"], "Hallo");
    }

    #[test]
    fn set_block_rows_empty_clears_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT,
                _locale TEXT
            );"
        ).unwrap();

        let blocks = vec![serde_json::json!({"_block_type": "text", "body": "Hi"})];
        set_block_rows(&conn, "posts", "content", "p1", &blocks, Some("en")).unwrap();

        // Clearing with empty slice should remove only the en locale rows
        set_block_rows(&conn, "posts", "content", "p1", &[], Some("en")).unwrap();
        let en = find_block_rows(&conn, "posts", "content", "p1", Some("en")).unwrap();
        assert!(en.is_empty());
    }

    // ── reconstruct_group_fields: nested Group branch ────────────────────

    #[test]
    fn hydrate_nested_group_fields() {
        let conn = Connection::open_in_memory().unwrap();

        // seo__social__og_title is a nested group column: group "seo" → sub-group "social" → field "og_title"
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                seo__social__og_title TEXT
            );
            INSERT INTO posts VALUES ('p1', 'OG Title Value');"
        ).unwrap();

        let inner_group = FieldDefinition {
            name: "social".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition { name: "og_title".to_string(), ..Default::default() },
            ],
            ..Default::default()
        };
        let outer_group = FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![inner_group],
            ..Default::default()
        };

        let fields = vec![outer_group];

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("seo__social__og_title".to_string(), serde_json::json!("OG Title Value"));

        hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

        // Should be reconstructed as nested: { seo: { social: { og_title: "..." } } }
        let seo = doc.fields.get("seo").expect("seo group should exist");
        let social = seo.get("social").expect("nested social group should exist");
        assert_eq!(social.get("og_title").and_then(|v| v.as_str()), Some("OG Title Value"));
    }

    // ── reconstruct_group_fields: Row sub-field inside Group ─────────────

    #[test]
    fn hydrate_group_with_row_sub_fields() {
        // A Row inside a Group is transparent — its sub-fields are promoted to the group level
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             INSERT INTO posts (id) VALUES ('p1');"
        ).unwrap();

        let row_wrapper = FieldDefinition {
            name: "layout_row".to_string(),
            field_type: FieldType::Row,
            fields: vec![
                FieldDefinition { name: "col_a".to_string(), ..Default::default() },
                FieldDefinition { name: "col_b".to_string(), ..Default::default() },
            ],
            ..Default::default()
        };
        let outer_group = FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Group,
            fields: vec![row_wrapper],
            ..Default::default()
        };

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("layout__col_a".to_string(), serde_json::json!("A"));
        doc.fields.insert("layout__col_b".to_string(), serde_json::json!("B"));

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        // Both sub-fields should appear at the group level (not nested in a "layout_row" key)
        let layout = doc.fields.get("layout").expect("layout group should exist");
        assert_eq!(layout.get("col_a").and_then(|v| v.as_str()), Some("A"));
        assert_eq!(layout.get("col_b").and_then(|v| v.as_str()), Some("B"));
        assert!(layout.get("layout_row").is_none(), "Row wrapper should be transparent");
    }

    // ── reconstruct_group_fields: Tabs sub-field inside Group ─────────────

    #[test]
    fn hydrate_group_with_tabs_sub_fields() {
        use crate::core::field::FieldTab;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             INSERT INTO posts (id) VALUES ('p1');"
        ).unwrap();

        let tabs_wrapper = FieldDefinition {
            name: "tabs".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Tab A".to_string(),
                    description: None,
                    fields: vec![FieldDefinition { name: "field_a".to_string(), ..Default::default() }],
                },
                FieldTab {
                    label: "Tab B".to_string(),
                    description: None,
                    fields: vec![FieldDefinition { name: "field_b".to_string(), ..Default::default() }],
                },
            ],
            ..Default::default()
        };
        let outer_group = FieldDefinition {
            name: "settings".to_string(),
            field_type: FieldType::Group,
            fields: vec![tabs_wrapper],
            ..Default::default()
        };

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("settings__field_a".to_string(), serde_json::json!("val_a"));
        doc.fields.insert("settings__field_b".to_string(), serde_json::json!("val_b"));

        hydrate_document(&conn, "posts", &[outer_group], &mut doc, None, None).unwrap();

        let settings = doc.fields.get("settings").expect("settings group should exist");
        assert_eq!(settings.get("field_a").and_then(|v| v.as_str()), Some("val_a"));
        assert_eq!(settings.get("field_b").and_then(|v| v.as_str()), Some("val_b"));
    }

    // ── hydrate_document: locale fallback for has-many ───────────────────

    #[test]
    fn hydrate_fallback_locale_for_has_many() {
        use crate::config::LocaleConfig;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_tags (
                 parent_id TEXT,
                 related_id TEXT,
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             -- Only 'en' locale data exists, no 'de'
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 't1', 0, 'en');"
        ).unwrap();

        let tags_field = FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            localized: true,
            relationship: Some(RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        };

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = super::LocaleContext {
            mode: super::LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &[tags_field], &mut doc, None, Some(&locale_ctx)).unwrap();

        // "de" has no data, should fall back to "en" results
        let tags = doc.fields.get("tags").expect("tags should be hydrated via fallback");
        let arr = tags.as_array().expect("should be array");
        assert_eq!(arr.len(), 1, "should fall back to 'en' when 'de' is empty");
        assert_eq!(arr[0].as_str(), Some("t1"));
    }

    // ── hydrate_document: locale fallback for arrays ─────────────────────

    #[test]
    fn hydrate_fallback_locale_for_arrays() {
        use crate::config::LocaleConfig;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_items (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 label TEXT,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_items (id, parent_id, _order, label, _locale) VALUES ('i1', 'p1', 0, 'EN Item', 'en');"
        ).unwrap();

        let items_field = FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            localized: true,
            fields: vec![FieldDefinition { name: "label".to_string(), ..Default::default() }],
            ..Default::default()
        };

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = super::LocaleContext {
            mode: super::LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &[items_field], &mut doc, None, Some(&locale_ctx)).unwrap();

        // "de" has no array data, should fall back to "en"
        let items = doc.fields.get("items").expect("items should be hydrated via fallback");
        let arr = items.as_array().expect("should be array");
        assert_eq!(arr.len(), 1, "should fall back to 'en' items when 'de' is empty");
        assert_eq!(arr[0]["label"], "EN Item");
    }

    // ── hydrate_document: locale fallback for blocks ──────────────────────

    #[test]
    fn hydrate_fallback_locale_for_blocks() {
        use crate::config::LocaleConfig;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_content (
                 id TEXT PRIMARY KEY,
                 parent_id TEXT,
                 _order INTEGER,
                 _block_type TEXT,
                 data TEXT,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_content (id, parent_id, _order, _block_type, data, _locale)
                 VALUES ('b1', 'p1', 0, 'text', '{\"body\":\"EN Content\"}', 'en');"
        ).unwrap();

        let content_field = FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            localized: true,
            ..Default::default()
        };

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = super::LocaleContext {
            mode: super::LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &[content_field], &mut doc, None, Some(&locale_ctx)).unwrap();

        // "de" has no blocks, should fall back to "en"
        let content = doc.fields.get("content").expect("content should be hydrated via fallback");
        let arr = content.as_array().expect("should be array");
        assert_eq!(arr.len(), 1, "should fall back to 'en' blocks when 'de' is empty");
        assert_eq!(arr[0]["_block_type"], "text");
    }

    // ── hydrate_document: fallback not triggered when non-empty ──────────

    #[test]
    fn hydrate_fallback_not_triggered_when_data_exists() {
        use crate::config::LocaleConfig;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_tags (
                 parent_id TEXT,
                 related_id TEXT,
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 'de_tag1', 0, 'de');
             INSERT INTO posts_tags (parent_id, related_id, _order, _locale) VALUES ('p1', 'en_tag1', 0, 'en');"
        ).unwrap();

        let tags_field = FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            localized: true,
            relationship: Some(RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        };

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = super::LocaleContext {
            mode: super::LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &[tags_field], &mut doc, None, Some(&locale_ctx)).unwrap();

        // "de" HAS data — should NOT fall back to "en"
        let tags = doc.fields.get("tags").unwrap();
        let arr = tags.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("de_tag1"), "should use de data, not fall back to en");
    }

    // ── hydrate_document: polymorphic fallback ───────────────────────────

    #[test]
    fn hydrate_fallback_locale_for_polymorphic_has_many() {
        use crate::config::LocaleConfig;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY);
             CREATE TABLE posts_refs (
                 parent_id TEXT,
                 related_id TEXT,
                 related_collection TEXT NOT NULL DEFAULT '',
                 _order INTEGER,
                 _locale TEXT
             );
             INSERT INTO posts (id) VALUES ('p1');
             INSERT INTO posts_refs (parent_id, related_id, related_collection, _order, _locale)
                 VALUES ('p1', 'a1', 'articles', 0, 'en');"
        ).unwrap();

        let refs_field = FieldDefinition {
            name: "refs".to_string(),
            field_type: FieldType::Relationship,
            localized: true,
            relationship: Some(RelationshipConfig {
                collection: "articles".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec!["articles".to_string(), "pages".to_string()],
            }),
            ..Default::default()
        };

        let locale_config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = super::LocaleContext {
            mode: super::LocaleMode::Single("de".to_string()),
            config: locale_config,
        };

        let mut doc = Document::new("p1".to_string());
        hydrate_document(&conn, "posts", &[refs_field], &mut doc, None, Some(&locale_ctx)).unwrap();

        // "de" has no polymorphic refs, should fall back to "en"
        let refs = doc.fields.get("refs").expect("refs should be hydrated via fallback");
        let arr = refs.as_array().expect("should be array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_str(), Some("articles/a1"));
    }

    // ── save_join_table_data: non-array value for blocks ─────────────────

    #[test]
    fn save_join_table_data_blocks_non_array_is_noop() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        // Passing a non-array value for blocks should produce no rows
        let mut data = HashMap::new();
        data.insert("content".to_string(), serde_json::json!("not an array"));
        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
        assert!(found.is_empty());
    }

    // ── save_join_table_data: has-many from non-string/non-array value ────

    #[test]
    fn save_join_table_data_has_many_non_string_non_array_is_empty() {
        let conn = setup_join_db();
        let def = posts_def_with_joins();

        // Passing a number for has-many should yield no IDs (falls through to Vec::new())
        let mut data = HashMap::new();
        data.insert("tags".to_string(), serde_json::json!(42));
        save_join_table_data(&conn, "posts", &def.fields, "p1", &data, None).unwrap();

        let found = find_related_ids(&conn, "posts", "tags", "p1", None).unwrap();
        assert!(found.is_empty());
    }
}
