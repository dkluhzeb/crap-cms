//! Scan array sub-fields and blocks sub-fields for relationship/upload
//! columns that point at the target document.

use crate::core::{
    BlockDefinition, FieldDefinition, FieldType,
    field::{flatten_array_sub_fields, to_title_case},
};

use super::helpers::{field_display_label, query_ids_simple, query_ids_simple_params};
use super::types::{BackRefScan, BackReference};
use crate::db::DbValue;

/// Scan array sub-fields for relationship/upload fields (uses `flatten_array_sub_fields` logic).
pub(super) fn scan_array_sub_fields(
    scan: &BackRefScan,
    fields: &[FieldDefinition],
    array_table: &str,
    array_field_name: &str,
    results: &mut Vec<BackReference>,
) {
    let flat = flatten_array_sub_fields(fields);
    for sub in flat {
        match sub.field_type {
            FieldType::Relationship | FieldType::Upload => {
                let rc = match &sub.relationship {
                    Some(rc) if rc.all_collections().contains(&scan.target_collection) => rc,
                    _ => continue,
                };

                if rc.has_many {
                    // Has-many inside array — junction table named {array_table}_{sub.name}
                    // This is unusual but theoretically possible. Skip for now.
                    continue;
                }

                let match_value = if rc.is_polymorphic() {
                    format!("{}/{}", scan.target_collection, scan.target_id)
                } else {
                    scan.target_id.to_string()
                };

                let p1 = scan.conn.placeholder(1);
                let sql = format!(
                    "SELECT DISTINCT parent_id FROM \"{}\" WHERE \"{}\" = {p1}",
                    array_table, sub.name
                );
                let ids = query_ids_simple(scan.conn, &sql, &match_value);

                if !ids.is_empty() {
                    let label = format!(
                        "{} > {}",
                        to_title_case(array_field_name),
                        field_display_label(sub)
                    );
                    results.push(BackReference::new(
                        scan.owner_slug.to_string(),
                        scan.owner_label.to_string(),
                        format!("{}.{}", array_field_name, sub.name),
                        label,
                        ids,
                        scan.is_global,
                    ));
                }
            }
            _ => {}
        }
    }
}

/// Scan blocks sub-fields for relationship/upload fields.
pub(super) fn scan_blocks(
    scan: &BackRefScan,
    blocks: &[BlockDefinition],
    blocks_table: &str,
    blocks_field_name: &str,
    results: &mut Vec<BackReference>,
) {
    for block in blocks {
        let flat = flatten_array_sub_fields(&block.fields);
        for sub in &flat {
            match sub.field_type {
                FieldType::Relationship | FieldType::Upload => {
                    let rc = match &sub.relationship {
                        Some(rc) if rc.all_collections().contains(&scan.target_collection) => rc,
                        _ => continue,
                    };

                    if rc.has_many {
                        continue; // has-many inside blocks not supported for scan
                    }

                    let match_value = if rc.is_polymorphic() {
                        format!("{}/{}", scan.target_collection, scan.target_id)
                    } else {
                        scan.target_id.to_string()
                    };

                    let extract = scan.conn.json_extract_expr("data", &sub.name);
                    let (p1, p2) = (scan.conn.placeholder(1), scan.conn.placeholder(2));
                    let sql = format!(
                        "SELECT DISTINCT parent_id FROM \"{blocks_table}\" WHERE _block_type = {p1} AND {extract} = {p2}"
                    );
                    let params = vec![
                        DbValue::Text(block.block_type.clone()),
                        DbValue::Text(match_value),
                    ];
                    let ids = query_ids_simple_params(scan.conn, &sql, &params);

                    if !ids.is_empty() {
                        let label = format!(
                            "{} > {} > {}",
                            to_title_case(blocks_field_name),
                            block.label.as_ref().map_or_else(
                                || to_title_case(&block.block_type),
                                |l| l.resolve_default().to_string()
                            ),
                            field_display_label(sub),
                        );
                        results.push(BackReference::new(
                            scan.owner_slug.to_string(),
                            scan.owner_label.to_string(),
                            format!("{}.{}.{}", blocks_field_name, block.block_type, sub.name),
                            label,
                            ids,
                            scan.is_global,
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
}
