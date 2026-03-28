//! `find_by_id`, `find_by_ids`, `find_by_id_raw` — single and batch document lookup.

use anyhow::{Context as _, Result};

use crate::{
    core::{CollectionDefinition, Document},
    db::{
        DbConnection, DbValue, LocaleContext, LocaleMode,
        document::row_to_document,
        query::{get_column_names, get_locale_select_columns_with_opts, group_locale_fields},
    },
};

/// Find a single document by ID with full hydration (join tables, group reconstruction).
///
/// This is the standard read function — returns a fully hydrated document with
/// nested group objects and populated join table data (arrays, blocks, relationships).
/// Use `find_by_id_raw` when you only need flat column data without hydration.
///
/// Soft-deleted documents are excluded by default when `def.soft_delete` is true.
/// Use [`find_by_id_unfiltered`] to include soft-deleted documents.
pub fn find_by_id(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<Document>> {
    let doc = find_by_id_raw(conn, slug, def, id, locale_ctx)?;
    match doc {
        Some(mut d) => {
            super::super::hydrate_document(conn, slug, &def.fields, &mut d, None, locale_ctx)?;
            Ok(Some(d))
        }
        None => Ok(None),
    }
}

/// Like [`find_by_id`] but includes soft-deleted documents.
///
/// Used by trash/restore operations that need to access deleted documents.
pub fn find_by_id_unfiltered(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<Document>> {
    let doc = find_by_id_raw_unfiltered(conn, slug, def, id, locale_ctx)?;
    match doc {
        Some(mut d) => {
            super::super::hydrate_document(conn, slug, &def.fields, &mut d, None, locale_ctx)?;
            Ok(Some(d))
        }
        None => Ok(None),
    }
}

/// Find multiple documents by IDs with full hydration.
///
/// Uses a single `SELECT ... WHERE id IN (?, ?, ...)` query instead of N individual
/// lookups. Returns documents in arbitrary order (caller should reorder if needed).
/// Missing IDs are silently skipped (not included in the result).
pub fn find_by_ids(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    ids: &[String],
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<Document>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let (select_exprs, _result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => {
            get_locale_select_columns_with_opts(&def.fields, def.timestamps, def.soft_delete, ctx)?
        }
        _ => {
            let names = get_column_names(def);
            (names.clone(), names)
        }
    };

    let placeholders: Vec<String> = (1..=ids.len()).map(|i| conn.placeholder(i)).collect();
    let mut sql = format!(
        "SELECT {} FROM {} WHERE id IN ({})",
        select_exprs.join(", "),
        slug,
        placeholders.join(", ")
    );

    if def.soft_delete {
        sql.push_str(" AND _deleted_at IS NULL");
    }

    let params: Vec<DbValue> = ids.iter().map(|id| DbValue::Text(id.clone())).collect();
    let rows = conn
        .query_all(&sql, &params)
        .with_context(|| format!("Failed to execute find_by_ids on '{slug}'"))?;

    let mut documents = Vec::new();
    for row in &rows {
        let mut doc = row_to_document(conn, row)?;

        if let Some(ctx) = locale_ctx
            && ctx.config.is_enabled()
            && let LocaleMode::All = ctx.mode
        {
            group_locale_fields(&mut doc, &def.fields, &ctx.config)?;
        }
        super::super::hydrate_document(conn, slug, &def.fields, &mut doc, None, locale_ctx)?;
        documents.push(doc);
    }

    Ok(documents)
}

/// Find a single document by ID without hydration (raw column data only).
///
/// Returns flat column data as stored in the parent table. Group fields remain
/// as `field__subfield` flat keys. Join table data (arrays, blocks, relationships)
/// is NOT populated. Used internally by write operations that don't need hydration.
///
/// Soft-deleted documents are excluded by default when `def.soft_delete` is true.
/// Use [`find_by_id_raw_unfiltered`] when you need to access soft-deleted documents
/// (e.g., for the trash view or restore operations).
pub(crate) fn find_by_id_raw(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<Document>> {
    find_by_id_raw_inner(conn, slug, def, id, locale_ctx, false)
}

/// Like [`find_by_id_raw`] but includes soft-deleted documents.
///
/// Used by trash/restore operations that need to access deleted documents.
pub(crate) fn find_by_id_raw_unfiltered(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Option<Document>> {
    find_by_id_raw_inner(conn, slug, def, id, locale_ctx, true)
}

fn find_by_id_raw_inner(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    locale_ctx: Option<&LocaleContext>,
    include_deleted: bool,
) -> Result<Option<Document>> {
    let (select_exprs, _result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => {
            get_locale_select_columns_with_opts(&def.fields, def.timestamps, def.soft_delete, ctx)?
        }
        _ => {
            let names = get_column_names(def);
            (names.clone(), names)
        }
    };

    let mut sql = format!(
        "SELECT {} FROM {} WHERE id = {}",
        select_exprs.join(", "),
        slug,
        conn.placeholder(1)
    );

    if def.soft_delete && !include_deleted {
        sql.push_str(" AND _deleted_at IS NULL");
    }

    let row = conn
        .query_one(&sql, &[DbValue::Text(id.to_string())])
        .with_context(|| format!("Failed to find document {id} in {slug}"))?;

    match row {
        None => Ok(None),
        Some(r) => {
            let mut doc = row_to_document(conn, &r)?;
            if let Some(ctx) = locale_ctx
                && ctx.config.is_enabled()
                && let LocaleMode::All = ctx.mode
            {
                group_locale_fields(&mut doc, &def.fields, &ctx.config)?;
            }
            Ok(Some(doc))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::write::create;
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig};
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{DbPool, pool};
    use std::collections::{HashMap, HashSet};
    use tempfile::TempDir;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def
    }

    fn setup_db() -> (TempDir, DbPool) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        db_pool
            .get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE posts (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    status TEXT,
                    created_at TEXT,
                    updated_at TEXT
                )",
            )
            .unwrap();
        (tmp, db_pool)
    }

    #[test]
    fn find_by_id_exists() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Test Post".to_string());
        data.insert("status".to_string(), "draft".to_string());
        let created = create(&conn, "posts", &def, &data, None).unwrap();

        let found = find_by_id(&conn, "posts", &def, &created.id, None).unwrap();
        assert!(found.is_some());
        let doc = found.unwrap();
        assert_eq!(doc.id, created.id);
        assert_eq!(doc.get_str("title"), Some("Test Post"));
        assert_eq!(doc.get_str("status"), Some("draft"));
    }

    #[test]
    fn find_by_id_not_found() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let found = find_by_id(&conn, "posts", &def, "nonexistent-id", None).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_by_ids_empty_returns_empty() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();
        let result = find_by_ids(&conn, "posts", &def, &[], None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn find_by_ids_returns_matching() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "First".to_string());
        let doc1 = create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = HashMap::new();
        d2.insert("title".to_string(), "Second".to_string());
        let doc2 = create(&conn, "posts", &def, &d2, None).unwrap();

        let mut d3 = HashMap::new();
        d3.insert("title".to_string(), "Third".to_string());
        create(&conn, "posts", &def, &d3, None).unwrap();

        // Fetch only first two
        let ids = vec![doc1.id.to_string(), doc2.id.to_string()];
        let result = find_by_ids(&conn, "posts", &def, &ids, None).unwrap();
        assert_eq!(result.len(), 2);

        let titles: HashSet<String> = result
            .iter()
            .filter_map(|d| d.get_str("title").map(|s| s.to_string()))
            .collect();
        assert!(titles.contains("First"));
        assert!(titles.contains("Second"));
        assert!(!titles.contains("Third"));
    }

    // ── Soft-delete filtering tests ───────────────────────────────────────

    fn setup_soft_delete_db() -> (TempDir, DbPool) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        db_pool
            .get()
            .unwrap()
            .execute_batch(
                "CREATE TABLE articles (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    status TEXT,
                    _deleted_at TEXT,
                    created_at TEXT,
                    updated_at TEXT
                )",
            )
            .unwrap();
        (tmp, db_pool)
    }

    fn soft_delete_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("articles");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def.soft_delete = true;
        def
    }

    #[test]
    fn find_by_id_excludes_soft_deleted() {
        let (_tmp, pool) = setup_soft_delete_db();
        let conn = pool.get().unwrap();

        conn.execute(
            "INSERT INTO articles (id, title, status, _deleted_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            &[
                DbValue::Text("id-deleted".into()),
                DbValue::Text("Gone".into()),
                DbValue::Text("draft".into()),
                DbValue::Text("2026-01-02 00:00:00".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        let def = soft_delete_def();
        let found = find_by_id(&conn, "articles", &def, "id-deleted", None).unwrap();
        assert!(
            found.is_none(),
            "Soft-deleted doc should not be found by find_by_id"
        );
    }

    #[test]
    fn find_by_id_unfiltered_includes_soft_deleted() {
        let (_tmp, pool) = setup_soft_delete_db();
        let conn = pool.get().unwrap();

        conn.execute(
            "INSERT INTO articles (id, title, status, _deleted_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            &[
                DbValue::Text("id-deleted".into()),
                DbValue::Text("Gone".into()),
                DbValue::Text("draft".into()),
                DbValue::Text("2026-01-02 00:00:00".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        let def = soft_delete_def();
        let found =
            super::find_by_id_unfiltered(&conn, "articles", &def, "id-deleted", None).unwrap();
        assert!(
            found.is_some(),
            "find_by_id_unfiltered should include soft-deleted docs"
        );
        assert_eq!(found.unwrap().get_str("title"), Some("Gone"));
    }

    #[test]
    fn find_by_ids_missing_ids_skipped() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "Exists".to_string());
        let doc1 = create(&conn, "posts", &def, &d1, None).unwrap();

        let ids = vec![doc1.id.to_string(), "nonexistent-id".to_string()];
        let result = find_by_ids(&conn, "posts", &def, &ids, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, doc1.id);
    }
}
