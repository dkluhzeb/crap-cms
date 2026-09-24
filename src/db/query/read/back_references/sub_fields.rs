//! Scan array and blocks join tables for relationship/upload references that
//! point at the target document — at any nesting depth.
//!
//! Both scanners load the join-table rows and walk each through the shared
//! [`walk_nested_with`] / [`walk_blocks_with`] primitive (the same one
//! ref-counting uses), so they agree on exactly which references exist —
//! including relationships nested in a group inside an array, has-many inside
//! an array/blocks, and array-in-array. The walk reports the field path to
//! each match, which is rendered into the `field_name` / label.

use std::collections::HashSet;

use tracing::{debug, warn};

use super::{
    helpers::field_display_label,
    types::{BackRefScan, BackReference},
};
use crate::{
    core::{BLOCK_TYPE_KEY, FieldDefinition, NestStep, any_field},
    db::query::{
        join::find_all_array_rows_with_parent,
        ref_count::{walk_blocks_with, walk_nested_with},
    },
};

/// How one referring field inside a row is reported: its display path, its
/// query path, and its human label.
struct RowFieldKey {
    field_name: String,
    query_path: String,
    label: String,
}

/// Accumulates matched parent document ids per discovered field path, keeping
/// insertion order and deduplicating `(field_name, parent_id)` pairs.
struct PathAccumulator {
    entries: Vec<(RowFieldKey, Vec<String>)>,
    seen: HashSet<(String, String)>,
}

impl PathAccumulator {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn record(&mut self, key: RowFieldKey, parent_id: &str) {
        if !self
            .seen
            .insert((key.field_name.clone(), parent_id.to_string()))
        {
            return;
        }

        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|(k, _)| k.field_name == key.field_name)
        {
            entry.1.push(parent_id.to_string());
        } else {
            self.entries.push((key, vec![parent_id.to_string()]));
        }
    }

    fn into_references(self, scan: &BackRefScan) -> Vec<BackReference> {
        self.entries
            .into_iter()
            .map(|(key, ids)| {
                BackReference::builder(scan.owner_slug, key.field_name)
                    .owner_label(scan.owner_label)
                    .field_label(key.label)
                    .query_path(key.query_path)
                    .document_ids(ids)
                    .global(scan.is_global)
                    .build()
            })
            .collect()
    }
}

/// The report key of `leaf`, reached through `path` inside the rows of the
/// field at document path `root` (labelled `prefix`).
fn row_field_key(
    root: &str,
    prefix: &str,
    path: &[NestStep<'_>],
    leaf: &FieldDefinition,
) -> RowFieldKey {
    RowFieldKey {
        field_name: format!("{root}.{}", path_names(path, leaf, true)),
        query_path: format!("{root}.{}", path_names(path, leaf, false)),
        label: build_label(prefix, path, leaf),
    }
}

/// Whether any relationship/upload field in this subtree could target
/// `target` — used to skip loading join tables that can't contribute.
fn fields_may_target(fields: &[FieldDefinition], target: &str) -> bool {
    any_field(fields, &|f| {
        f.field_type.is_reference()
            && f.relationship
                .as_ref()
                .is_some_and(|rc| rc.all_collections().contains(&target))
    })
}

/// The dotted path below a row: ancestor segment names plus the leaf field.
/// A block row's type is a segment of the display path (`with_block_types`)
/// but not of a query path, which never names it.
fn path_names(path: &[NestStep<'_>], leaf: &FieldDefinition, with_block_types: bool) -> String {
    let mut parts: Vec<&str> = path
        .iter()
        .filter_map(|seg| match seg {
            NestStep::Field(f) => Some(f.name.as_str()),
            NestStep::Block(b) => with_block_types.then_some(b.block_type.as_str()),
        })
        .collect();
    parts.push(leaf.name.as_str());

    parts.join(".")
}

/// The human label: container prefix, then each ancestor segment's display
/// label, then the leaf field's label — joined with ` > `.
fn build_label(prefix: &str, path: &[NestStep<'_>], leaf: &FieldDefinition) -> String {
    let mut parts: Vec<String> = vec![prefix.to_string()];

    for seg in path {
        parts.push(match seg {
            NestStep::Field(f) => field_display_label(f),
            NestStep::Block(b) => b.display_label(),
        });
    }
    parts.push(field_display_label(leaf));

    parts.join(" > ")
}

/// Whether `parent_id` is the target document itself (excluded from its own
/// back-references, except for globals which have a single fixed row).
fn is_self_reference(scan: &BackRefScan, parent_id: &str) -> bool {
    !scan.is_global && scan.owner_slug == scan.target_collection && parent_id == scan.target_id
}

/// Scan an array join table for references at any depth inside its rows.
/// `root` is the array field's dotted document path (`meta.items`).
pub(super) fn scan_array_sub_fields(
    scan: &BackRefScan,
    field: &FieldDefinition,
    array_table: &str,
    root: &str,
) -> Vec<BackReference> {
    if !fields_may_target(&field.fields, scan.target_collection) {
        return Vec::new();
    }

    let rows = match find_all_array_rows_with_parent(scan.conn, array_table, &field.fields) {
        Ok(rows) => rows,
        Err(e) => {
            debug!("Back-ref array scan skipping {}: {}", array_table, e);
            return Vec::new();
        }
    };

    let mut acc = PathAccumulator::new();
    let prefix = field_display_label(field);

    for (parent_id, row) in &rows {
        if is_self_reference(scan, parent_id) {
            continue;
        }

        let mut stack = Vec::new();
        walk_nested_with(
            row,
            &field.fields,
            &mut stack,
            &mut |leaf, path, coll, id, _poly| {
                if coll == scan.target_collection && id == scan.target_id {
                    acc.record(row_field_key(root, &prefix, path, leaf), parent_id);
                }
            },
        );
    }

    acc.into_references(scan)
}

/// Scan a blocks join table for references at any depth inside block data.
/// `root` is the blocks field's dotted document path (`meta.content`).
pub(super) fn scan_blocks(
    scan: &BackRefScan,
    field: &FieldDefinition,
    blocks_table: &str,
    root: &str,
) -> Vec<BackReference> {
    if !field
        .blocks
        .iter()
        .any(|b| fields_may_target(&b.fields, scan.target_collection))
    {
        return Vec::new();
    }

    let sql = format!("SELECT parent_id, _block_type, data FROM \"{blocks_table}\"");
    let db_rows = match scan.conn.query_all(&sql, &[]) {
        Ok(rows) => rows,
        Err(e) => {
            debug!("Back-ref blocks scan skipping {}: {}", blocks_table, e);
            return Vec::new();
        }
    };

    let mut acc = PathAccumulator::new();
    let prefix = field_display_label(field);

    for db_row in &db_rows {
        let Some(parent_id) = db_row.opt_text_at(0) else {
            continue;
        };
        if is_self_reference(scan, &parent_id) {
            continue;
        }

        let block_type = db_row.opt_text_at(1).unwrap_or_default();
        let data = db_row.opt_text_at(2).unwrap_or_default();

        let mut obj = serde_json::from_str::<serde_json::Value>(&data)
            .inspect_err(|e| {
                warn!("Back-ref blocks scan: malformed data JSON in {blocks_table}: {e}");
            })
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        obj.insert(
            BLOCK_TYPE_KEY.to_string(),
            serde_json::Value::String(block_type),
        );

        let instances = [serde_json::Value::Object(obj)];
        let mut stack = Vec::new();
        walk_blocks_with(
            &instances,
            &field.blocks,
            &mut stack,
            &mut |leaf, path, coll, id, _poly| {
                if coll == scan.target_collection && id == scan.target_id {
                    acc.record(row_field_key(root, &prefix, path, leaf), &parent_id);
                }
            },
        );
    }

    acc.into_references(scan)
}

#[cfg(test)]
mod tests {
    use crate::core::CollectionDefinition;
    use crate::core::field::*;
    use crate::db::DbConnection;
    use crate::db::query::read::back_references::find_back_references;
    use crate::db::query::read::back_references::test_helpers::*;

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
            &[],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].field_name, "slides.image");
        assert_eq!(refs[0].count, 1);
    }

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
            &[],
        ).unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].field_name, "content.hero.bg_image");
        // A query path never names the block type.
        assert_eq!(refs[0].query_path, "content.bg_image");
        assert_eq!(refs[0].count, 1);
    }

    /// Regression: Array field inside a Group must use the group-prefixed
    /// junction table name (e.g. `posts_meta__items`), not `posts_items`.
    #[test]
    fn group_nested_array_uses_prefixed_junction_table() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("items", FieldType::Array)
                        .fields(vec![
                            FieldDefinition::builder("image", FieldType::Upload)
                                .relationship(RelationshipConfig::new("media", false))
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");

        // The migration creates `posts_meta__items` (group-prefixed).
        conn.execute(
            "INSERT INTO posts_meta__items (parent_id, image, _order) VALUES (?1, ?2, 0)",
            &[
                crate::db::DbValue::Text("p1".into()),
                crate::db::DbValue::Text("m1".into()),
            ],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(
            refs.len(),
            1,
            "should find back-ref through group-nested array"
        );
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].count, 1);

        // Regression: reported without its group (`items.image`), so the
        // report's field could not be judged against the group's access.
        assert_eq!(refs[0].field_name, "meta.items.image");
        assert_eq!(refs[0].query_path, "meta.items.image");
    }

    /// Regression: a relationship nested in a Group *inside* an array row used
    /// to be invisible to the back-ref scanner (it flattened away the group),
    /// even though ref-counting counted it. Now both walk the same JSON.
    #[test]
    fn relationship_in_group_inside_array_row_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
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

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");
        // The group inside the array row is stored as JSON in the `meta` column.
        conn.execute(
            "INSERT INTO posts_slides (id, parent_id, _order, meta) VALUES ('s1', 'p1', 0, '{\"image\":\"m1\"}')",
            &[],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1, "group-in-array relationship must be found");
        assert_eq!(refs[0].field_name, "slides.meta.image");
        assert_eq!(refs[0].count, 1);
    }

    /// Regression: array-in-array — a relationship in an inner array nested in
    /// an outer array row. The inner array is JSON in the outer row's column.
    #[test]
    fn relationship_in_array_inside_array_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![
                            FieldDefinition::builder("image", FieldType::Upload)
                                .relationship(RelationshipConfig::new("media", false))
                                .build(),
                        ])
                        .build(),
                ])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");
        conn.execute(
            "INSERT INTO posts_outer (id, parent_id, _order, inner) VALUES ('o1', 'p1', 0, '[{\"image\":\"m1\"}]')",
            &[],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1, "array-in-array relationship must be found");
        assert_eq!(refs[0].field_name, "outer.inner.image");
        assert_eq!(refs[0].count, 1);
    }

    /// Regression: a relationship nested in a Group inside a block used to be
    /// missed (the blocks scanner only looked one `json_extract` deep).
    #[test]
    fn relationship_in_group_inside_block_found() {
        let media = CollectionDefinition::new("media");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("bg", FieldType::Upload)
                                    .relationship(RelationshipConfig::new("media", false))
                                    .build(),
                            ])
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
            "INSERT INTO posts_content (id, parent_id, _order, _block_type, data) VALUES ('b1', 'p1', 0, 'hero', '{\"meta\":{\"bg\":\"m1\"}}')",
            &[],
        ).unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1, "group-in-block relationship must be found");
        assert_eq!(refs[0].field_name, "content.hero.meta.bg");
        assert_eq!(refs[0].query_path, "content.meta.bg");
        assert_eq!(refs[0].count, 1);
    }

    /// Regression: a has-many relationship inside an array row used to be
    /// skipped by the scanner ("unusual... skip for now") even though
    /// ref-counting counted it. Now both agree.
    #[test]
    fn has_many_relationship_inside_array_row_found() {
        let tags = CollectionDefinition::new("tags");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("rows", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("tags", FieldType::Relationship)
                        .relationship(RelationshipConfig::new("tags", true))
                        .build(),
                ])
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[tags, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "tags", "t1");
        insert_doc(&conn, "posts", "p1");
        // has-many inside an array row is stored as a JSON array in the column.
        conn.execute(
            "INSERT INTO posts_rows (id, parent_id, _order, tags) VALUES ('r1', 'p1', 0, '[\"t1\"]')",
            &[],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "tags", "t1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1, "has-many in array must be found");
        assert_eq!(refs[0].field_name, "rows.tags");
        assert_eq!(refs[0].count, 1);
    }
}
