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

use crate::core::{FieldDefinition, FieldType, NestStep, any_field};
use crate::db::query::join::find_all_array_rows_with_parent;
use crate::db::query::ref_count::{walk_blocks_with, walk_nested_with};

use super::helpers::field_display_label;
use super::types::{BackRefScan, BackReference};

/// Accumulates matched parent document ids per discovered field path, keeping
/// insertion order and deduplicating `(field_name, parent_id)` pairs.
struct PathAccumulator {
    entries: Vec<(String, String, Vec<String>)>,
    seen: HashSet<(String, String)>,
}

impl PathAccumulator {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn record(&mut self, field_name: String, label: String, parent_id: &str) {
        if !self
            .seen
            .insert((field_name.clone(), parent_id.to_string()))
        {
            return;
        }

        if let Some(entry) = self.entries.iter_mut().find(|(n, ..)| *n == field_name) {
            entry.2.push(parent_id.to_string());
        } else {
            self.entries
                .push((field_name, label, vec![parent_id.to_string()]));
        }
    }

    fn drain_into(self, scan: &BackRefScan, results: &mut Vec<BackReference>) {
        for (field_name, label, ids) in self.entries {
            results.push(BackReference::new(
                scan.owner_slug.to_string(),
                scan.owner_label.to_string(),
                field_name,
                label,
                ids,
                scan.is_global,
            ));
        }
    }
}

/// Whether any relationship/upload field in this subtree could target
/// `target` — used to skip loading join tables that can't contribute.
fn fields_may_target(fields: &[FieldDefinition], target: &str) -> bool {
    any_field(fields, &|f| {
        matches!(f.field_type, FieldType::Relationship | FieldType::Upload)
            && f.relationship
                .as_ref()
                .is_some_and(|rc| rc.all_collections().contains(&target))
    })
}

/// The dotted `field_name` path: ancestor segment names plus the leaf field.
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
pub(super) fn scan_array_sub_fields(
    scan: &BackRefScan,
    field: &FieldDefinition,
    array_table: &str,
    results: &mut Vec<BackReference>,
) {
    if !fields_may_target(&field.fields, scan.target_collection) {
        return;
    }

    let rows = match find_all_array_rows_with_parent(scan.conn, array_table, &field.fields) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!("Back-ref array scan skipping {}: {}", array_table, e);
            return;
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
                    let field_name = format!("{}.{}", field.name, path_names(path, leaf));
                    acc.record(field_name, build_label(&prefix, path, leaf), parent_id);
                }
            },
        );
    }

    acc.drain_into(scan, results);
}

/// Scan a blocks join table for references at any depth inside block data.
pub(super) fn scan_blocks(
    scan: &BackRefScan,
    field: &FieldDefinition,
    blocks_table: &str,
    results: &mut Vec<BackReference>,
) {
    if !field
        .blocks
        .iter()
        .any(|b| fields_may_target(&b.fields, scan.target_collection))
    {
        return;
    }

    let sql = format!("SELECT parent_id, _block_type, data FROM \"{blocks_table}\"");
    let db_rows = match scan.conn.query_all(&sql, &[]) {
        Ok(rows) => rows,
        Err(e) => {
            debug!("Back-ref blocks scan skipping {}: {}", blocks_table, e);
            return;
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
            "_block_type".to_string(),
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
                    let field_name = format!("{}.{}", field.name, path_names(path, leaf));
                    acc.record(field_name, build_label(&prefix, path, leaf), &parent_id);
                }
            },
        );
    }

    acc.drain_into(scan, results);
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
