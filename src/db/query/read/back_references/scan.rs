//! Top-level [`find_back_references`] orchestrator and the relationship-field
//! scanners (has-one column / has-many junction).

use anyhow::Result;

use crate::config::LocaleConfig;
use crate::core::{FieldChildren, FieldDefinition, FieldType, Registry, field_children};
use crate::db::query::helpers::{
    global_table, join_table, locale_column, prefixed_name as prefixed,
};
use crate::db::query::poly_ref;
use crate::db::{DbConnection, DbValue};

use super::helpers::query_ids;
use super::sub_fields::{scan_array_sub_fields, scan_blocks};
use super::types::{BackRefScan, BackReference};

use super::helpers::field_display_label;

/// Scan all collections and globals for back-references to `target_id` in `target_collection`.
///
/// # Errors
///
/// Returns a backend error if any of the back-reference queries fails.
pub fn find_back_references(
    conn: &dyn DbConnection,
    registry: &Registry,
    target_collection: &str,
    target_id: &str,
    locale_config: &LocaleConfig,
) -> Result<Vec<BackReference>> {
    let mut results = Vec::new();

    // Scan collections
    for (slug, def) in &registry.collections {
        let table: &str = slug;
        let scan = BackRefScan {
            conn,
            target_collection,
            target_id,
            locale_config,
            owner_slug: slug,
            owner_label: def.display_name(),
            is_global: false,
        };
        scan_fields(&scan, &def.fields, table, "", &mut results)?;
    }

    // Scan globals
    for (slug, def) in &registry.globals {
        let table = global_table(slug);
        let scan = BackRefScan {
            conn,
            target_collection,
            target_id,
            locale_config,
            owner_slug: slug,
            owner_label: def.display_name(),
            is_global: true,
        };
        scan_fields(&scan, &def.fields, &table, "", &mut results)?;
    }

    Ok(results)
}

/// Recursively walk a field tree, matching the same recursion pattern as
/// `collect_column_specs_inner` in `src/db/migrate/helpers.rs`.
fn scan_fields(
    scan: &BackRefScan,
    fields: &[FieldDefinition],
    parent_table: &str,
    prefix: &str,
    results: &mut Vec<BackReference>,
) -> Result<()> {
    for field in fields {
        match field_children(field) {
            FieldChildren::Group(sub) => {
                scan_fields(
                    scan,
                    sub,
                    parent_table,
                    &prefixed(prefix, &field.name),
                    results,
                )?;
            }
            FieldChildren::Wrapper(sub) => {
                scan_fields(scan, sub, parent_table, prefix, results)?;
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    scan_fields(scan, &tab.fields, parent_table, prefix, results)?;
                }
            }
            FieldChildren::Array(_) => {
                let table = join_table(parent_table, &prefixed(prefix, &field.name));
                scan_array_sub_fields(scan, field, &table, results);
            }
            FieldChildren::Blocks(_) => {
                let table = join_table(parent_table, &prefixed(prefix, &field.name));
                scan_blocks(scan, field, &table, results);
            }
            // Relationship/Upload leaves carry an incoming reference to scan;
            // scalars and the virtual Join carry none.
            FieldChildren::Leaf => match field.field_type {
                FieldType::Relationship | FieldType::Upload => {
                    scan_relationship(scan, field, parent_table, prefix, results)?;
                }
                _ => {}
            },
        }
    }

    Ok(())
}

/// Scan a single relationship/upload field for back-references.
fn scan_relationship(
    scan: &BackRefScan,
    field: &FieldDefinition,
    parent_table: &str,
    prefix: &str,
    results: &mut Vec<BackReference>,
) -> Result<()> {
    let rc = match &field.relationship {
        Some(rc) if rc.all_collections().contains(&scan.target_collection) => rc,
        _ => return Ok(()),
    };

    let col = prefixed(prefix, &field.name);
    let field_label = field_display_label(field);

    let ids = if field.has_parent_column() {
        query_has_one(
            scan,
            parent_table,
            &col,
            rc.is_polymorphic(),
            field.localized && scan.locale_config.is_enabled(),
        )?
    } else {
        let junction = join_table(parent_table, &col);
        query_has_many(
            scan.conn,
            &junction,
            scan.target_collection,
            scan.target_id,
            rc.is_polymorphic(),
        )
    };

    if !ids.is_empty() {
        results.push(BackReference::new(
            scan.owner_slug.to_string(),
            scan.owner_label.to_string(),
            field.name.clone(),
            field_label,
            ids,
            scan.is_global,
        ));
    }

    Ok(())
}

/// Query has-one relationship column for a reference.
fn query_has_one(
    scan: &BackRefScan,
    table: &str,
    col: &str,
    is_polymorphic: bool,
    is_localized: bool,
) -> Result<Vec<String>> {
    if is_localized {
        // Localized has-one: check all locale columns
        let locale_cols: Vec<String> = scan
            .locale_config
            .locales
            .iter()
            .map(|l| locale_column(col, l))
            .collect::<Result<Vec<String>>>()?;

        if locale_cols.is_empty() {
            return Ok(Vec::new());
        }

        let match_value = if is_polymorphic {
            poly_ref::format(scan.target_collection, scan.target_id)
        } else {
            scan.target_id.to_string()
        };

        let p1 = scan.conn.placeholder(1);
        let conditions: Vec<String> = locale_cols
            .iter()
            .map(|c| format!("\"{c}\" = {p1}"))
            .collect();
        let sql = format!(
            "SELECT id FROM \"{}\" WHERE {}",
            table,
            conditions.join(" OR ")
        );
        Ok(query_ids(
            scan.conn,
            &sql,
            &[DbValue::Text(match_value)],
            scan.owner_slug,
            scan.target_id,
            scan.target_collection,
            scan.is_global,
        ))
    } else if is_polymorphic {
        let match_value = poly_ref::format(scan.target_collection, scan.target_id);
        let p1 = scan.conn.placeholder(1);
        let sql = format!("SELECT id FROM \"{table}\" WHERE \"{col}\" = {p1}");
        Ok(query_ids(
            scan.conn,
            &sql,
            &[DbValue::Text(match_value)],
            scan.owner_slug,
            scan.target_id,
            scan.target_collection,
            scan.is_global,
        ))
    } else {
        let p1 = scan.conn.placeholder(1);
        let sql = format!("SELECT id FROM \"{table}\" WHERE \"{col}\" = {p1}");
        Ok(query_ids(
            scan.conn,
            &sql,
            &[DbValue::Text(scan.target_id.to_string())],
            scan.owner_slug,
            scan.target_id,
            scan.target_collection,
            scan.is_global,
        ))
    }
}

/// Query has-many junction table for references.
fn query_has_many(
    conn: &dyn DbConnection,
    junction_table: &str,
    target_collection: &str,
    target_id: &str,
    is_polymorphic: bool,
) -> Vec<String> {
    if is_polymorphic {
        let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
        let sql = format!(
            "SELECT DISTINCT parent_id FROM \"{junction_table}\" WHERE related_id = {p1} AND related_collection = {p2}"
        );
        let params = vec![
            DbValue::Text(target_id.to_string()),
            DbValue::Text(target_collection.to_string()),
        ];
        match conn.query_all(&sql, &params) {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| row.opt_text_at(0))
                .collect(),
            Err(e) => {
                tracing::debug!("Back-ref scan skipping {}: {}", junction_table, e);
                Vec::new()
            }
        }
    } else {
        let p1 = conn.placeholder(1);
        let sql =
            format!("SELECT DISTINCT parent_id FROM \"{junction_table}\" WHERE related_id = {p1}");
        let params = vec![DbValue::Text(target_id.to_string())];
        match conn.query_all(&sql, &params) {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| row.opt_text_at(0))
                .collect(),
            Err(e) => {
                tracing::debug!("Back-ref scan skipping {}: {}", junction_table, e);
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Slug;
    use crate::core::{CollectionDefinition, GlobalDefinition};
    use crate::core::{FieldDefinition, FieldType, RelationshipConfig};
    use crate::db::query::read::back_references::test_helpers::*;

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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].field_name, "image");
        assert_eq!(refs[0].count, 2);
        assert!(refs[0].document_ids.contains(&"p1".to_string()));
        assert!(refs[0].document_ids.contains(&"p2".to_string()));
    }

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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert!(refs.is_empty());
    }

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
            &[DbValue::Text("p1".into()), DbValue::Text("t1".into())],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES (?1, ?2, 0)",
            &[DbValue::Text("p2".into()), DbValue::Text("t1".into())],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "tags", "t1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].count, 2);
    }

    #[test]
    fn polymorphic_has_one_finds_back_reference() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("featured", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: false,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, pages, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc_with_field(&conn, "posts", "p1", "featured", "media/m1");

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].count, 1);
    }

    #[test]
    fn polymorphic_has_many_finds_back_reference() {
        let media = CollectionDefinition::new("media");
        let pages = CollectionDefinition::new("pages");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(RelationshipConfig {
                    collection: Slug::new("media"),
                    has_many: true,
                    max_depth: None,
                    polymorphic: vec![Slug::new("media"), Slug::new("pages")],
                })
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[media, pages, posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        insert_doc(&conn, "media", "m1");
        insert_doc(&conn, "posts", "p1");
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES (?1, ?2, ?3, 0)",
            &[DbValue::Text("p1".into()), DbValue::Text("m1".into()), DbValue::Text("media".into())],
        ).unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].count, 1);
    }

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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].field_name, "hero");
    }

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
        conn.execute(
            "UPDATE _global_settings SET logo = ?1",
            &[DbValue::Text("m1".into())],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].owner_slug, "settings");
        assert!(refs[0].is_global);
    }

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
        conn.execute("INSERT INTO posts (id, hero__en) VALUES ('p1', 'm1')", &[])
            .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &locale).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].count, 1);
    }

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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 2);
        let slugs: Vec<&str> = refs.iter().map(|r| r.owner_slug.as_str()).collect();
        assert!(slugs.contains(&"posts"));
        assert!(slugs.contains(&"pages"));
    }

    /// Regression: when a collection has a self-referencing relationship
    /// (e.g. posts -> posts) and a document references itself, the
    /// back-references for that document must NOT include itself.
    #[test]
    fn self_reference_excluded_from_back_references() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("related_post", FieldType::Relationship)
                .relationship(RelationshipConfig::new("posts", false))
                .build(),
        ];

        let (_tmp, pool, registry) = setup_db(&[posts], &[], &no_locale());
        let conn = pool.get().unwrap();

        // p1 references itself, p2 references p1
        insert_doc_with_field(&conn, "posts", "p1", "related_post", "p1");
        insert_doc_with_field(&conn, "posts", "p2", "related_post", "p1");

        let refs = find_back_references(&conn, &registry, "posts", "p1", &no_locale()).unwrap();

        // Only p2 should appear as a back-reference, not p1 (self)
        assert_eq!(refs.len(), 1, "should have exactly one back-ref group");
        assert_eq!(refs[0].owner_slug, "posts");
        assert!(
            !refs[0].document_ids.contains(&"p1".to_string()),
            "self-reference p1 should be filtered out, got: {:?}",
            refs[0].document_ids
        );
        assert!(
            refs[0].document_ids.contains(&"p2".to_string()),
            "p2 should be in back-references"
        );
        assert_eq!(refs[0].count, 1);
    }

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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert!(refs.is_empty());
    }

    /// Regression: has-many relationship inside a Group must use the
    /// group-prefixed junction table name (e.g. `posts_meta__tags`),
    /// not just `posts_tags`.
    #[test]
    fn group_nested_has_many_uses_prefixed_junction_table() {
        let tags = CollectionDefinition::new("tags");
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
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

        // The migration creates `posts_meta__tags` (group-prefixed), not `posts_tags`.
        conn.execute(
            "INSERT INTO posts_meta__tags (parent_id, related_id, _order) VALUES (?1, ?2, 0)",
            &[DbValue::Text("p1".into()), DbValue::Text("t1".into())],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "tags", "t1", &no_locale()).unwrap();
        assert_eq!(
            refs.len(),
            1,
            "should find back-ref through group-nested has-many"
        );
        assert_eq!(refs[0].owner_slug, "posts");
        assert_eq!(refs[0].count, 1);
    }
}
