//! Scan all collections and globals for documents that reference a given target document.
//! Also: check version snapshots for missing (deleted) relationship targets.

use serde::Serialize;

use anyhow::Result;

use crate::{
    config::LocaleConfig,
    core::{
        BlockDefinition, FieldDefinition, FieldType, Registry,
        field::{flatten_array_sub_fields, to_title_case},
    },
    db::{DbConnection, DbValue},
};

use crate::db::query::helpers::{
    global_table, join_table, locale_column, prefixed_name as prefixed,
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
    conn: &'a dyn DbConnection,
    target_collection: &'a str,
    target_id: &'a str,
    locale_config: &'a LocaleConfig,
    owner_slug: &'a str,
    owner_label: &'a str,
    is_global: bool,
}

/// Scan all collections and globals for back-references to `target_id` in `target_collection`.
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
        match field.field_type {
            FieldType::Group => {
                scan_fields(
                    scan,
                    &field.fields,
                    parent_table,
                    &prefixed(prefix, &field.name),
                    results,
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                scan_fields(scan, &field.fields, parent_table, prefix, results)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    scan_fields(scan, &tab.fields, parent_table, prefix, results)?;
                }
            }
            FieldType::Relationship | FieldType::Upload => {
                scan_relationship(scan, field, parent_table, prefix, results)?;
            }
            FieldType::Array => {
                let table = join_table(parent_table, &prefixed(prefix, &field.name));
                scan_array_sub_fields(scan, &field.fields, &table, &field.name, results);
            }
            FieldType::Blocks => {
                let table = join_table(parent_table, &prefixed(prefix, &field.name));
                scan_blocks(scan, &field.blocks, &table, &field.name, results);
            }
            _ => {}
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
            format!("{}/{}", scan.target_collection, scan.target_id)
        } else {
            scan.target_id.to_string()
        };

        let p1 = scan.conn.placeholder(1);
        let conditions: Vec<String> = locale_cols
            .iter()
            .map(|c| format!("\"{}\" = {p1}", c))
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
        let match_value = format!("{}/{}", scan.target_collection, scan.target_id);
        let p1 = scan.conn.placeholder(1);
        let sql = format!("SELECT id FROM \"{}\" WHERE \"{}\" = {p1}", table, col);
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
        let sql = format!("SELECT id FROM \"{}\" WHERE \"{}\" = {p1}", table, col);
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
            "SELECT DISTINCT parent_id FROM \"{}\" WHERE related_id = {p1} AND related_collection = {p2}",
            junction_table
        );
        let params = vec![
            DbValue::Text(target_id.to_string()),
            DbValue::Text(target_collection.to_string()),
        ];
        match conn.query_all(&sql, &params) {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| {
                    if let Some(DbValue::Text(s)) = row.get_value(0) {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            Err(e) => {
                tracing::debug!("Back-ref scan skipping {}: {}", junction_table, e);
                Vec::new()
            }
        }
    } else {
        let p1 = conn.placeholder(1);
        let sql = format!(
            "SELECT DISTINCT parent_id FROM \"{}\" WHERE related_id = {p1}",
            junction_table
        );
        let params = vec![DbValue::Text(target_id.to_string())];
        match conn.query_all(&sql, &params) {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| {
                    if let Some(DbValue::Text(s)) = row.get_value(0) {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            Err(e) => {
                tracing::debug!("Back-ref scan skipping {}: {}", junction_table, e);
                Vec::new()
            }
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

                    let extract = scan.conn.json_extract_expr("data", &sub.name);
                    let (p1, p2) = (scan.conn.placeholder(1), scan.conn.placeholder(2));
                    let sql = format!(
                        "SELECT DISTINCT parent_id FROM \"{}\" WHERE _block_type = {p1} AND {} = {p2}",
                        blocks_table, extract
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
    conn: &dyn DbConnection,
    sql: &str,
    params: &[DbValue],
    owner_slug: &str,
    target_id: &str,
    target_collection: &str,
    is_global: bool,
) -> Vec<String> {
    match conn.query_all(sql, params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                if let Some(DbValue::Text(s)) = row.get_value(0) {
                    Some(s.clone())
                } else {
                    None
                }
            })
            // Skip self-references (same collection, same ID)
            .filter(|id| is_global || id != target_id || owner_slug != target_collection)
            .collect(),
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            Vec::new()
        }
    }
}

/// Simple query for array/blocks parent_id lookups.
fn query_ids_simple(conn: &dyn DbConnection, sql: &str, value: &str) -> Vec<String> {
    let params = vec![DbValue::Text(value.to_string())];
    match conn.query_all(sql, &params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                if let Some(DbValue::Text(s)) = row.get_value(0) {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .collect(),
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            Vec::new()
        }
    }
}

/// Query with arbitrary params, returning collected IDs.
fn query_ids_simple_params(conn: &dyn DbConnection, sql: &str, params: &[DbValue]) -> Vec<String> {
    match conn.query_all(sql, params) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|row| {
                if let Some(DbValue::Text(s)) = row.get_value(0) {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .collect(),
        Err(e) => {
            tracing::debug!("Back-ref scan query failed: {}", e);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig, LocaleConfig};
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::core::{Registry, Slug};
    use crate::db::{DbConnection, DbPool, DbValue, migrate, pool};

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

    fn insert_doc(conn: &dyn DbConnection, table: &str, id: &str) {
        conn.execute(
            &format!("INSERT INTO \"{}\" (id) VALUES (?1)", table),
            &[DbValue::Text(id.to_string())],
        )
        .unwrap();
    }

    fn insert_doc_with_field(conn: &dyn DbConnection, table: &str, id: &str, col: &str, val: &str) {
        conn.execute(
            &format!(
                "INSERT INTO \"{}\" (id, \"{}\") VALUES (?1, ?2)",
                table, col
            ),
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(val.to_string()),
            ],
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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
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
            &[DbValue::Text("p1".into()), DbValue::Text("m1".into()), DbValue::Text("media".into())],
        ).unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
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
            &[],
        )
        .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
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
            &[],
        ).unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
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
        conn.execute("INSERT INTO posts (id, hero__en) VALUES ('p1', 'm1')", &[])
            .unwrap();

        let refs = find_back_references(&conn, &registry, "media", "m1", &locale).unwrap();
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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert_eq!(refs.len(), 2);
        let slugs: Vec<&str> = refs.iter().map(|r| r.owner_slug.as_str()).collect();
        assert!(slugs.contains(&"posts"));
        assert!(slugs.contains(&"pages"));
    }

    // ── Self-referencing collection ─────────────────────────────────────

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

        let refs = find_back_references(&conn, &registry, "media", "m1", &no_locale()).unwrap();
        assert!(refs.is_empty());
    }

    // ── Group-nested has-many uses correct junction table name ────────

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

    // ── Group-nested array uses correct junction table name ──────────

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
            &[DbValue::Text("p1".into()), DbValue::Text("m1".into())],
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
