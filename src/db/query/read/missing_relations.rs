//! Check a version's documents for relationship/upload fields whose targets no
//! longer exist.

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

use tracing::debug;

use crate::{
    core::{
        BlockDefinition, Document, FieldChildren, FieldDefinition, NestStep, Registry,
        RelationshipConfig, field::to_title_case, field_children,
    },
    db::{
        DbConnection, DbValue,
        query::{
            helpers::placeholder_list,
            poly_ref,
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

/// Check documents for relationship/upload fields whose targets no longer exist.
///
/// `views` are read-shaped documents — groups nested, localized values resolved
/// — so a stored snapshot is checked as one view per locale it records. Each
/// field is reported once under its dotted path (`meta.hero`, `slides.image`),
/// its referenced ids combined across the views and each counted once.
pub fn find_missing_relations(
    conn: &dyn DbConnection,
    registry: &Registry,
    views: &[Document],
    fields: &[FieldDefinition],
) -> Vec<MissingRelation> {
    let mut acc = MissingAcc::new();
    let root = ScanPrefix::root();

    for view in views {
        let obj: Map<String, Value> = view
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        collect_refs(&obj, fields, &root, &mut acc);
    }

    acc.drain(conn, registry)
}

/// Where a scan stands in the field tree: the dotted field path and the display
/// label of the level being scanned, both empty at the document root.
struct ScanPrefix {
    name: String,
    label: String,
}

impl ScanPrefix {
    fn new(name: String, label: String) -> Self {
        Self { name, label }
    }

    fn root() -> Self {
        Self::new(String::new(), String::new())
    }

    /// The level inside a group field.
    fn group(&self, field: &FieldDefinition) -> Self {
        Self::new(
            self.name_of(&field.name),
            self.label_of(&field_display_label(field)),
        )
    }

    /// The level of an array or blocks field's rows, labelled by its
    /// title-cased name.
    fn rows(&self, field: &FieldDefinition) -> Self {
        Self::new(
            self.name_of(&field.name),
            self.label_of(&to_title_case(&field.name)),
        )
    }

    /// `name` appended to the dotted path.
    fn name_of(&self, name: &str) -> String {
        if self.name.is_empty() {
            return name.to_string();
        }

        format!("{}.{name}", self.name)
    }

    /// `label` appended to the label, joined with ` > `.
    fn label_of(&self, label: &str) -> String {
        if self.label.is_empty() {
            return label.to_string();
        }

        format!("{} > {label}", self.label)
    }
}

/// Walk the field tree over one level of a view, recording every reference.
fn collect_refs<'a>(
    obj: &Map<String, Value>,
    fields: &'a [FieldDefinition],
    prefix: &ScanPrefix,
    acc: &mut MissingAcc<'a>,
) {
    for field in fields {
        match field_children(field) {
            FieldChildren::Group(sub) => {
                if let Some(nested) = obj.get(&field.name).and_then(Value::as_object) {
                    collect_refs(nested, sub, &prefix.group(field), acc);
                }
            }
            FieldChildren::Wrapper(sub) => collect_refs(obj, sub, prefix, acc),
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    collect_refs(obj, &tab.fields, prefix, acc);
                }
            }
            FieldChildren::Array(sub) => {
                if let Some(rows) = obj.get(&field.name).and_then(Value::as_array) {
                    collect_array_refs(rows, sub, &prefix.rows(field), acc);
                }
            }
            FieldChildren::Blocks(blocks) => {
                if let Some(rows) = obj.get(&field.name).and_then(Value::as_array) {
                    collect_blocks_refs(rows, blocks, &prefix.rows(field), acc);
                }
            }
            FieldChildren::Leaf => collect_leaf_refs(obj.get(&field.name), field, prefix, acc),
        }
    }
}

/// Record a relationship/upload leaf's references; every other leaf has none.
fn collect_leaf_refs<'a>(
    value: Option<&Value>,
    field: &'a FieldDefinition,
    prefix: &ScanPrefix,
    acc: &mut MissingAcc<'a>,
) {
    let Some(rc) = &field.relationship else {
        return;
    };

    for ref_id in extract_ref_ids(value, rc.is_polymorphic()) {
        let name = prefix.name_of(&field.name);
        let label = prefix.label_of(&field_display_label(field));

        acc.add(name, label, rc, ref_id);
    }
}

/// Record the references in array rows at ANY nesting depth (a relationship in
/// a group inside the row, an inner array, etc.), via the shared
/// [`walk_nested_with`] — the same walker ref-counting and back-references use,
/// so the three agree on which references a row contains.
fn collect_array_refs<'a>(
    rows: &[Value],
    fields: &'a [FieldDefinition],
    prefix: &ScanPrefix,
    acc: &mut MissingAcc<'a>,
) {
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
                acc.add_nested(prefix, leaf, path, (coll.to_string(), id.to_string()));
            },
        );
    }
}

/// Record the references in blocks rows at any nesting depth, via the shared
/// [`walk_blocks_with`].
fn collect_blocks_refs<'a>(
    rows: &[Value],
    blocks: &'a [BlockDefinition],
    prefix: &ScanPrefix,
    acc: &mut MissingAcc<'a>,
) {
    let mut stack = Vec::new();

    walk_blocks_with(
        rows,
        blocks,
        &mut stack,
        &mut |leaf, path, coll, id, _poly| {
            acc.add_nested(prefix, leaf, path, (coll.to_string(), id.to_string()));
        },
    );
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

/// Parse a single reference ID string, returning (collection, id). A
/// non-polymorphic ref is the bare id (empty collection); a polymorphic ref
/// goes through the shared `poly_ref` grammar so it rejects a malformed
/// `"col/"` / `"/id"` the same way every other reader does.
fn parse_ref_id(s: &str, is_polymorphic: bool) -> Option<(String, String)> {
    if !is_polymorphic {
        return Some((String::new(), s.to_string()));
    }

    poly_ref::parse(s)
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
            poly_ref::format(collection, id)
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
    let sql = format!(
        "SELECT id FROM \"{}\" WHERE id IN ({})",
        collection,
        placeholder_list(conn, ids.len())
    );
    let params: Vec<DbValue> = ids.iter().map(|s| DbValue::Text(s.clone())).collect();
    match conn.query_all(&sql, &params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| row.opt_text_at(0))
            .collect(),
        Err(e) => {
            debug!("Missing relations check skipping {}: {}", collection, e);
            HashSet::new()
        }
    }
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

/// The human label: each ancestor segment's display label, then the leaf
/// field's label — joined with ` > `.
fn path_label(path: &[NestStep<'_>], leaf: &FieldDefinition) -> String {
    let mut parts: Vec<String> = path
        .iter()
        .map(|seg| match seg {
            NestStep::Field(f) => field_display_label(f),
            NestStep::Block(b) => b.display_label(),
        })
        .collect();
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
/// across all rows and views (one `MissingRelation` per path), keeping
/// insertion order.
struct MissingAcc<'a> {
    entries: Vec<MissingEntry<'a>>,
}

impl<'a> MissingAcc<'a> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Record one `(collection, id)` reference of a field path. A path keeps
    /// each id once, however many rows or views hold it.
    fn add(
        &mut self,
        field_name: String,
        label: String,
        rc: &'a RelationshipConfig,
        ref_id: (String, String),
    ) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.field_name == field_name) {
            if !entry.ids.contains(&ref_id) {
                entry.ids.push(ref_id);
            }

            return;
        }

        self.entries.push(MissingEntry {
            field_name,
            label,
            rc,
            ids: vec![ref_id],
        });
    }

    /// Record a reference a nested walk found at `path` below `prefix`.
    fn add_nested(
        &mut self,
        prefix: &ScanPrefix,
        leaf: &'a FieldDefinition,
        path: &[NestStep<'_>],
        ref_id: (String, String),
    ) {
        let Some(rc) = &leaf.relationship else {
            return;
        };

        let name = prefix.name_of(&path_names(path, leaf));
        let label = prefix.label_of(&path_label(path, leaf));

        self.add(name, label, rc, ref_id);
    }

    /// Check every recorded path's ids against the database.
    fn drain(self, conn: &dyn DbConnection, registry: &Registry) -> Vec<MissingRelation> {
        self.entries
            .into_iter()
            .filter_map(|entry| missing_relation(conn, registry, entry))
            .collect()
    }
}

/// The report for `entry` when any of its ids no longer exists.
fn missing_relation(
    conn: &dyn DbConnection,
    registry: &Registry,
    entry: MissingEntry<'_>,
) -> Option<MissingRelation> {
    let missing = check_ids_exist(conn, registry, &entry.ids, entry.rc);

    if missing.is_empty() {
        return None;
    }

    Some(MissingRelation::new(
        entry.field_name,
        entry.label,
        missing.into_iter().collect(),
        entry.ids.len(),
    ))
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

    /// A read-shaped view holding `snapshot`'s keys.
    fn view(snapshot: Value) -> Document {
        let mut doc = Document::new("p1".to_string());

        if let Value::Object(obj) = snapshot {
            doc.fields = obj.into_iter().collect();
        }

        doc
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].missing_ids.contains(&"media/m1".to_string()));
    }

    /// A relationship inside a group is reported under its dotted path and
    /// labelled through the group, like one inside an array or blocks row —
    /// a bare `hero` could not be told apart from a top-level `hero`.
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

        let snapshot = json!({"meta": {"hero": "m_gone"}});
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "meta.hero");
        assert_eq!(
            missing[0].field_label.split(" > ").count(),
            2,
            "the label names the group, then the field: {}",
            missing[0].field_label
        );
    }

    /// An array inside a group carries the group in its path too.
    #[test]
    fn missing_array_in_group_carries_the_group_path() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("slides", FieldType::Array)
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

        let snapshot = json!({"meta": {"slides": [{"image": "m_gone"}]}});
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "meta.slides.image");
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].field_name, "slides.image");
        assert_eq!(missing[0].field_label, "Slides > Image");
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
        assert_eq!(
            missing.len(),
            1,
            "a dangling ref in a group-in-array row must be detected"
        );
        assert_eq!(missing[0].field_name, "slides.meta.image");
        assert_eq!(missing[0].missing_count, 1);
        assert_eq!(missing[0].total_ids, 2);
    }

    /// The views of one snapshot (one per locale) report a field once: its ids
    /// combined across the views, a shared id counted once.
    #[test]
    fn a_field_is_reported_once_across_views() {
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

        let views = [
            view(json!({"tags": ["t1", "t_gone"]})),
            view(json!({"tags": ["t_gone", "t_also_gone"]})),
        ];
        let missing = find_missing_relations(&conn, &registry, &views, &fields);

        assert_eq!(missing.len(), 1, "one entry per field: {missing:?}");
        assert_eq!(missing[0].missing_count, 2);
        assert_eq!(missing[0].total_ids, 3);
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
        let missing = find_missing_relations(&conn, &registry, &[view(snapshot)], &fields);
        assert!(missing.is_empty());
    }
}
