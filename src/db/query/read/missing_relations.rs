//! Check version snapshots for relationship/upload fields whose targets no longer exist.

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

use crate::{
    core::{
        BlockDefinition, FieldDefinition, FieldType, NestStep, Registry, RelationshipConfig,
        field::to_title_case,
    },
    db::{
        DbConnection, DbValue,
        query::{
            helpers::prefixed_name,
            ref_count::{walk_blocks_with, walk_nested_with},
        },
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
    #[must_use]
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
    conn: &dyn DbConnection,
    registry: &Registry,
    snapshot: &Value,
    fields: &[FieldDefinition],
) -> Vec<MissingRelation> {
    let Some(obj) = snapshot.as_object() else {
        return Vec::new();
    };
    let mut results = Vec::new();
    collect_missing_fields(conn, registry, obj, fields, "", &mut results);
    results
}

/// Recursively walk the field tree and collect missing relations from the snapshot.
fn collect_missing_fields(
    conn: &dyn DbConnection,
    registry: &Registry,
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    prefix: &str,
    results: &mut Vec<MissingRelation>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);
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
                let Some(rc) = &field.relationship else {
                    continue;
                };

                let key = prefixed_name(prefix, &field.name);
                let val = obj.get(&key).or_else(|| obj.get(&field.name));
                let ids = extract_ref_ids(val, rc.is_polymorphic());
                let label = field_display_label(field);

                push_if_missing(conn, registry, &ids, rc, field.name.clone(), label, results);
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
    if !is_polymorphic {
        return Some((String::new(), s.to_string()));
    }

    let (col, id) = s.split_once('/')?;
    Some((col.to_string(), id.to_string()))
}

/// Check which IDs are missing from the database.
fn check_ids_exist(
    conn: &dyn DbConnection,
    registry: &Registry,
    ids: &[(String, String)],
    rc: &RelationshipConfig,
) -> HashSet<String> {
    // Group IDs by target collection
    let mut by_collection: HashMap<String, Vec<String>> = HashMap::new();
    for (col, id) in ids {
        let target = if col.is_empty() {
            rc.collection.to_string()
        } else {
            col.clone()
        };
        by_collection.entry(target).or_default().push(id.clone());
    }

    let display_id = |collection: &str, id: &str| -> String {
        if rc.is_polymorphic() {
            format!("{collection}/{id}")
        } else {
            id.to_string()
        }
    };

    let mut missing = HashSet::new();
    for (collection, check_ids) in &by_collection {
        if !registry.collections.contains_key(collection.as_str()) {
            missing.extend(check_ids.iter().map(|id| display_id(collection, id)));
            continue;
        }

        let existing = query_existing_ids(conn, collection, check_ids);
        for id in check_ids {
            if !existing.contains(id) {
                missing.insert(display_id(collection, id));
            }
        }
    }
    missing
}

/// Query which IDs exist in a collection table.
fn query_existing_ids(
    conn: &dyn DbConnection,
    collection: &str,
    ids: &[String],
) -> HashSet<String> {
    if ids.is_empty() {
        return HashSet::new();
    }
    let placeholders: Vec<String> = (1..=ids.len()).map(|i| conn.placeholder(i)).collect();
    let sql = format!(
        "SELECT id FROM \"{}\" WHERE id IN ({})",
        collection,
        placeholders.join(", ")
    );
    let params: Vec<DbValue> = ids.iter().map(|s| DbValue::Text(s.clone())).collect();
    match conn.query_all(&sql, &params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect(),
        Err(e) => {
            tracing::debug!("Missing relations check skipping {}: {}", collection, e);
            HashSet::new()
        }
    }
}

/// If any IDs are missing, push a `MissingRelation` onto results.
fn push_if_missing(
    conn: &dyn DbConnection,
    registry: &Registry,
    all_ids: &[(String, String)],
    rc: &RelationshipConfig,
    field_name: String,
    label: String,
    results: &mut Vec<MissingRelation>,
) {
    if all_ids.is_empty() {
        return;
    }

    let missing = check_ids_exist(conn, registry, all_ids, rc);
    if missing.is_empty() {
        return;
    }

    results.push(MissingRelation::new(
        field_name,
        label,
        missing.into_iter().collect(),
        all_ids.len(),
    ));
}

/// The dotted field path: ancestor segment names plus the leaf field name.
fn path_names(path: &[NestStep<'_>], leaf: &FieldDefinition) -> String {
    let mut parts: Vec<&str> = path
        .iter()
        .map(|seg| match seg {
            NestStep::Field(f) => f.name.as_str(),
            NestStep::Block(b) => b.block_type.as_str(),
        })
        .collect();
    parts.push(leaf.name.as_str());
    parts.join(".")
}

/// The human label: container prefix, each ancestor segment's display label,
/// then the leaf field's label — joined with ` > `.
fn path_label(prefix: &str, path: &[NestStep<'_>], leaf: &FieldDefinition) -> String {
    let mut parts: Vec<String> = vec![prefix.to_string()];
    for seg in path {
        parts.push(match seg {
            NestStep::Field(f) => field_display_label(f),
            NestStep::Block(b) => b.label.as_ref().map_or_else(
                || to_title_case(&b.block_type),
                |l| l.resolve_default().to_string(),
            ),
        });
    }
    parts.push(field_display_label(leaf));
    parts.join(" > ")
}

/// One discovered field path's referenced `(collection, id)` pairs, with the
/// leaf field's relationship config and display label.
struct MissingEntry<'a> {
    field_name: String,
    label: String,
    rc: &'a RelationshipConfig,
    ids: Vec<(String, String)>,
}

/// Accumulates [`MissingEntry`] per field path so the existence check aggregates
/// across all rows (one `MissingRelation` per path), keeping insertion order.
struct MissingAcc<'a> {
    entries: Vec<MissingEntry<'a>>,
}

impl<'a> MissingAcc<'a> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn add(
        &mut self,
        field_name: String,
        label: String,
        rc: &'a RelationshipConfig,
        coll: String,
        id: String,
    ) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.field_name == field_name) {
            e.ids.push((coll, id));
        } else {
            self.entries.push(MissingEntry {
                field_name,
                label,
                rc,
                ids: vec![(coll, id)],
            });
        }
    }

    fn drain(
        self,
        conn: &dyn DbConnection,
        registry: &Registry,
        results: &mut Vec<MissingRelation>,
    ) {
        for e in self.entries {
            push_if_missing(conn, registry, &e.ids, e.rc, e.field_name, e.label, results);
        }
    }
}

/// Check array rows for missing relations at ANY nesting depth (a relationship
/// in a group inside the row, an inner array, etc.), via the shared
/// [`walk_nested_with`] — the same walker ref-counting and back-references use,
/// so the three agree on which references a row contains.
fn collect_missing_in_array(
    conn: &dyn DbConnection,
    registry: &Registry,
    rows: &[Value],
    fields: &[FieldDefinition],
    array_name: &str,
    results: &mut Vec<MissingRelation>,
) {
    let mut acc = MissingAcc::new();
    let prefix = to_title_case(array_name);

    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };
        let mut stack = Vec::new();
        walk_nested_with(
            obj,
            fields,
            &mut stack,
            &mut |leaf, path, coll, id, _poly| {
                if let Some(rc) = &leaf.relationship {
                    acc.add(
                        format!("{}.{}", array_name, path_names(path, leaf)),
                        path_label(&prefix, path, leaf),
                        rc,
                        coll.to_string(),
                        id.to_string(),
                    );
                }
            },
        );
    }

    acc.drain(conn, registry, results);
}

/// Check blocks rows for missing relations at any nesting depth, via the shared
/// [`walk_blocks_with`].
fn collect_missing_in_blocks(
    conn: &dyn DbConnection,
    registry: &Registry,
    rows: &[Value],
    blocks: &[BlockDefinition],
    blocks_name: &str,
    results: &mut Vec<MissingRelation>,
) {
    let mut acc = MissingAcc::new();
    let prefix = to_title_case(blocks_name);

    let mut stack = Vec::new();
    walk_blocks_with(
        rows,
        blocks,
        &mut stack,
        &mut |leaf, path, coll, id, _poly| {
            if let Some(rc) = &leaf.relationship {
                acc.add(
                    format!("{}.{}", blocks_name, path_names(path, leaf)),
                    path_label(&prefix, path, leaf),
                    rc,
                    coll.to_string(),
                    id.to_string(),
                );
            }
        },
    );

    acc.drain(conn, registry, results);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        config::{CrapConfig, DatabaseConfig, LocaleConfig},
        core::{Registry, Slug, collection::*, field::*},
        db::{DbConnection, DbPool, DbValue, migrate, pool},
    };

    fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    fn setup_db(
        collections: &[CollectionDefinition],
        globals: &[GlobalDefinition],
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
        let registry = (*Registry::snapshot(&registry_shared)).clone();
        migrate::sync_all(&db_pool, &registry, locale).expect("sync");

        (tmp, db_pool, registry)
    }

    fn insert_doc(conn: &dyn DbConnection, table: &str, id: &str) {
        conn.execute(
            &format!("INSERT INTO \"{table}\" (id) VALUES (?1)"),
            &[DbValue::Text(id.to_string())],
        )
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
                    collection: Slug::new("media"),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
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

    /// Regression: a relationship in a Group *inside* an array row used to be
    /// invisible to the missing-relations scan (the flat walk never descended
    /// the group), so a dangling ref nested there wasn't flagged on the
    /// restore-confirm page even though ref-counting/back-refs saw it. The
    /// shared walker now descends it.
    #[test]
    fn missing_relation_in_group_inside_array_row() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("slides", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![
                            FieldDefinition::builder("image", FieldType::Upload)
                                .relationship(RelationshipConfig::new("media", false))
                                .build(),
                        ])
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
                { "meta": { "image": "m1" } },
                { "meta": { "image": "m_deleted" } }
            ]
        });
        let missing = find_missing_relations(&conn, &registry, &snapshot, &fields);
        assert_eq!(
            missing.len(),
            1,
            "a dangling ref in a group-in-array row must be detected"
        );
        assert_eq!(missing[0].field_name, "slides.meta.image");
        assert_eq!(missing[0].missing_count, 1);
        assert_eq!(missing[0].total_ids, 2);
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
