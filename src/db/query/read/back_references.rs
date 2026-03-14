//! Scan all collections and globals for documents that reference a given target document.
//! Also: check version snapshots for missing (deleted) relationship targets.

use serde::Serialize;

use crate::{
    config::LocaleConfig,
    core::{
        BlockDefinition, FieldDefinition, FieldType, Registry,
        field::{flatten_array_sub_fields, to_title_case},
    },
};

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

/// Invariant context for a back-reference scan operation.
struct BackRefScan<'a> {
    conn: &'a rusqlite::Connection,
    target_collection: &'a str,
    target_id: &'a str,
    locale_config: &'a LocaleConfig,
    owner_slug: &'a str,
    owner_label: &'a str,
    is_global: bool,
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
        scan_fields(&scan, &def.fields, table, "", &mut results);
    }

    // Scan globals
    for (slug, def) in &registry.globals {
        let table = format!("_global_{}", slug);
        let scan = BackRefScan {
            conn,
            target_collection,
            target_id,
            locale_config,
            owner_slug: slug,
            owner_label: def.display_name(),
            is_global: true,
        };
        scan_fields(&scan, &def.fields, &table, "", &mut results);
    }

    results
}

/// Recursively walk a field tree, matching the same recursion pattern as
/// `collect_column_specs_inner` in `src/db/migrate/helpers.rs`.
fn scan_fields(
    scan: &BackRefScan,
    fields: &[FieldDefinition],
    parent_table: &str,
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
                scan_fields(scan, &field.fields, parent_table, &new_prefix, results);
            }
            FieldType::Row | FieldType::Collapsible => {
                scan_fields(scan, &field.fields, parent_table, prefix, results);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    scan_fields(scan, &tab.fields, parent_table, prefix, results);
                }
            }
            FieldType::Relationship | FieldType::Upload => {
                let rc = match &field.relationship {
                    Some(rc) if rc.all_collections().contains(&scan.target_collection) => rc,
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
                        scan,
                        parent_table,
                        &col,
                        rc.is_polymorphic(),
                        field.localized && scan.locale_config.is_enabled(),
                    );

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
                } else {
                    // Has-many: junction table
                    let junction = format!("{}_{}", parent_table, field.name);
                    let ids = query_has_many(
                        scan.conn,
                        &junction,
                        scan.target_collection,
                        scan.target_id,
                        rc.is_polymorphic(),
                    );

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
                }
            }
            FieldType::Array => {
                let array_table = format!("{}_{}", parent_table, field.name);
                scan_array_sub_fields(scan, &field.fields, &array_table, &field.name, results);
            }
            FieldType::Blocks => {
                let blocks_table = format!("{}_{}", parent_table, field.name);
                scan_blocks(scan, &field.blocks, &blocks_table, &field.name, results);
            }
            _ => {}
        }
    }
}

/// Query has-one relationship column for a reference.
fn query_has_one(
    scan: &BackRefScan,
    table: &str,
    col: &str,
    is_polymorphic: bool,
    is_localized: bool,
) -> Vec<String> {
    if is_localized {
        // Localized has-one: check all locale columns
        let locale_cols: Vec<String> = scan
            .locale_config
            .locales
            .iter()
            .map(|l| format!("{}__{}", col, l))
            .collect();

        if locale_cols.is_empty() {
            return Vec::new();
        }

        let match_value = if is_polymorphic {
            format!("{}/{}", scan.target_collection, scan.target_id)
        } else {
            scan.target_id.to_string()
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
            scan.conn,
            &sql,
            &[&match_value],
            scan.owner_slug,
            scan.target_id,
            scan.is_global,
        )
    } else if is_polymorphic {
        let match_value = format!("{}/{}", scan.target_collection, scan.target_id);
        let sql = format!("SELECT id FROM \"{}\" WHERE \"{}\" = ?1", table, col);
        query_ids(
            scan.conn,
            &sql,
            &[&match_value as &dyn rusqlite::types::ToSql],
            scan.owner_slug,
            scan.target_id,
            scan.is_global,
        )
    } else {
        let sql = format!("SELECT id FROM \"{}\" WHERE \"{}\" = ?1", table, col);
        query_ids(
            scan.conn,
            &sql,
            &[&scan.target_id as &dyn rusqlite::types::ToSql],
            scan.owner_slug,
            scan.target_id,
            scan.is_global,
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
fn scan_array_sub_fields(
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

                let sql = format!(
                    "SELECT DISTINCT parent_id FROM \"{}\" WHERE \"{}\" = ?1",
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
fn scan_blocks(
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

                    let json_path = format!("$.{}", sub.name);
                    let sql = format!(
                        "SELECT DISTINCT parent_id FROM \"{}\" WHERE _block_type = ?1 AND json_extract(data, ?2) = ?3",
                        blocks_table
                    );
                    let ids = query_ids_blocks(
                        scan.conn,
                        &sql,
                        &block.block_type,
                        &json_path,
                        &match_value,
                    );

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

/// Get the display label for a field (admin label or title-cased name).
pub(super) fn field_display_label(field: &FieldDefinition) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig, LocaleConfig};
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::core::{Registry, Slug};
    use crate::db::{DbPool, migrate, pool};

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
}
