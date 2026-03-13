//! Check version snapshots for relationship/upload fields whose targets no longer exist.

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

use crate::core::{
    Registry,
    field::{
        BlockDefinition, FieldDefinition, FieldType, RelationshipConfig, flatten_array_sub_fields,
        to_title_case,
    },
};

use super::back_references::field_display_label;

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
    snapshot: &Value,
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
    obj: &Map<String, Value>,
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
fn extract_ref_ids(val: Option<&Value>, is_polymorphic: bool) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    match val {
        Some(Value::String(s)) if !s.is_empty() => {
            if let Some((col, id)) = parse_ref_id(s, is_polymorphic) {
                ids.push((col, id));
            }
        }
        Some(Value::Array(arr)) => {
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
    rc: &RelationshipConfig,
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
        if !registry.collections.contains_key(collection) {
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
    rows: &[Value],
    fields: &[FieldDefinition],
    array_name: &str,
    results: &mut Vec<MissingRelation>,
) {
    let flat = flatten_array_sub_fields(fields);
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
    rows: &[Value],
    blocks: &[BlockDefinition],
    blocks_name: &str,
    results: &mut Vec<MissingRelation>,
) {
    for block in blocks {
        let flat = flatten_array_sub_fields(&block.fields);
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
    use serde_json::json;

    use super::*;
    use crate::{
        config::{CrapConfig, DatabaseConfig, LocaleConfig},
        core::{Registry, collection::*, field::*},
        db::{DbPool, migrate, pool},
    };

    fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    fn setup_db(
        collections: &[CollectionDefinition],
        globals: &[crate::core::collection::GlobalDefinition],
        locale: &LocaleConfig,
    ) -> (tempfile::TempDir, DbPool, Registry) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
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

        let snapshot = json!({"title": "Hello", "image": "m_deleted"});
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

        let snapshot = json!({"image": "m1"});
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

        let snapshot = json!({"tags": ["t1", "t2"]});
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

        let snapshot = json!({"featured": "media/m1"});
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

        let snapshot = json!({"meta__hero": "m_gone"});
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

        let snapshot = json!({
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

        let snapshot = json!({
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

        let snapshot = json!({});
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert!(missing.is_empty());
    }
}
