//! `find_by_id`, `find_by_ids`, `find_by_id_raw` — single and batch document lookup.

use anyhow::{Context as _, Result};
use rusqlite::params_from_iter;

use crate::core::{CollectionDefinition, Document};
use crate::db::document::row_to_document;
use super::super::{
    LocaleMode, LocaleContext,
    get_column_names, get_locale_select_columns, group_locale_fields,
};

/// Find a single document by ID with full hydration (join tables, group reconstruction).
///
/// This is the standard read function — returns a fully hydrated document with
/// nested group objects and populated join table data (arrays, blocks, relationships).
/// Use `find_by_id_raw` when you only need flat column data without hydration.
pub fn find_by_id(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, id: &str, locale_ctx: Option<&LocaleContext>) -> Result<Option<Document>> {
    let doc = find_by_id_raw(conn, slug, def, id, locale_ctx)?;
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
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    ids: &[String],
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<Document>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let (select_exprs, result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => get_locale_select_columns(&def.fields, def.timestamps, ctx),
        _ => {
            let names = get_column_names(def);
            (names.clone(), names)
        }
    };

    let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT {} FROM {} WHERE id IN ({})",
        select_exprs.join(", "),
        slug,
        placeholders.join(", ")
    );

    let params: Vec<&dyn rusqlite::types::ToSql> = ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
    let mut stmt = conn.prepare(&sql)
        .with_context(|| format!("Failed to prepare find_by_ids query on '{}'", slug))?;

    let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
        row_to_document(row, &result_names)
    }).with_context(|| format!("Failed to execute find_by_ids on '{}'", slug))?;

    let mut documents = Vec::new();
    for row in rows {
        let mut doc = row?;
        if let Some(ctx) = locale_ctx {
            if ctx.config.is_enabled() {
                if let LocaleMode::All = ctx.mode {
                    group_locale_fields(&mut doc, &def.fields, &ctx.config);
                }
            }
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
pub(crate) fn find_by_id_raw(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, id: &str, locale_ctx: Option<&LocaleContext>) -> Result<Option<Document>> {
    let (select_exprs, result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => get_locale_select_columns(&def.fields, def.timestamps, ctx),
        _ => {
            let names = get_column_names(def);
            (names.clone(), names)
        }
    };

    let sql = format!("SELECT {} FROM {} WHERE id = ?1", select_exprs.join(", "), slug);

    let result = conn.query_row(&sql, [id], |row| {
        row_to_document(row, &result_names)
    });

    match result {
        Ok(mut doc) => {
            if let Some(ctx) = locale_ctx {
                if ctx.config.is_enabled() {
                    if let LocaleMode::All = ctx.mode {
                        group_locale_fields(&mut doc, &def.fields, &ctx.config);
                    }
                }
            }
            Ok(Some(doc))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to find document {} in {}", id, slug)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use crate::core::collection::*;
    use crate::core::field::*;
    use super::super::super::write::create;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition { name: "title".to_string(), ..Default::default() },
            FieldDefinition { name: "status".to_string(), ..Default::default() },
        ];
        def
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();
        conn
    }

    #[test]
    fn find_by_id_exists() {
        let conn = setup_db();
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
        let conn = setup_db();
        let def = test_def();

        let found = find_by_id(&conn, "posts", &def, "nonexistent-id", None).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn find_by_ids_empty_returns_empty() {
        let conn = setup_db();
        let def = test_def();
        let result = find_by_ids(&conn, "posts", &def, &[], None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn find_by_ids_returns_matching() {
        let conn = setup_db();
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
        let ids = vec![doc1.id.clone(), doc2.id.clone()];
        let result = find_by_ids(&conn, "posts", &def, &ids, None).unwrap();
        assert_eq!(result.len(), 2);

        let titles: HashSet<String> = result.iter()
            .filter_map(|d| d.get_str("title").map(|s| s.to_string()))
            .collect();
        assert!(titles.contains("First"));
        assert!(titles.contains("Second"));
        assert!(!titles.contains("Third"));
    }

    #[test]
    fn find_by_ids_missing_ids_skipped() {
        let conn = setup_db();
        let def = test_def();

        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "Exists".to_string());
        let doc1 = create(&conn, "posts", &def, &d1, None).unwrap();

        let ids = vec![doc1.id.clone(), "nonexistent-id".to_string()];
        let result = find_by_ids(&conn, "posts", &def, &ids, None).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, doc1.id);
    }
}
