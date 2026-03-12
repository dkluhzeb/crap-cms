//! `find()` — query multiple documents with filters, sorting, and cursor pagination.

use anyhow::{Context as _, Result, bail};
use rusqlite::params_from_iter;

use super::super::filter::{build_where_clause, resolve_filter_column, resolve_filters};
use super::super::{
    FindQuery, LocaleContext, LocaleMode, get_column_names, get_locale_select_columns,
    group_locale_fields, validate_query_fields,
};
use crate::core::{CollectionDefinition, Document};
use crate::db::document::row_to_document;

/// Convert ISO 8601 timestamp back to SQLite storage format for cursor comparison.
/// "2024-01-01T12:00:00.000Z" → "2024-01-01 12:00:00"
pub(super) fn denormalize_timestamp(s: &str) -> String {
    // Match the ISO format produced by normalize_timestamp in document.rs
    if s.len() == 24 && s.as_bytes().get(10) == Some(&b'T') && s.ends_with(".000Z") {
        format!("{} {}", &s[..10], &s[11..19])
    } else {
        s.to_string()
    }
}

/// Find documents matching a query.
pub fn find(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<Document>> {
    validate_query_fields(def, query, locale_ctx)?;

    let (select_exprs, result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => {
            get_locale_select_columns(&def.fields, def.timestamps, ctx)
        }
        _ => {
            let names = get_column_names(def);
            (names.clone(), names)
        }
    };

    let (select_exprs, result_names) =
        super::select::apply_select_filter(select_exprs, result_names, query.select.as_ref(), def);

    let mut sql = format!("SELECT {} FROM {}", select_exprs.join(", "), slug);
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // Build WHERE with locale-resolved column names
    let resolved_filters = resolve_filters(&query.filters, def, locale_ctx);
    let where_clause = build_where_clause(&resolved_filters, slug, &def.fields, &mut params)?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
    }

    // FTS5 full-text search filter
    if let Some(ref search_term) = query.search
        && let Some((fts_clause, sanitized)) =
            super::super::fts::fts_where_clause(conn, slug, search_term)
    {
        if where_clause.is_empty() {
            sql.push_str(&format!(" WHERE {}", fts_clause));
        } else {
            sql.push_str(&format!(" AND {}", fts_clause));
        }
        params.push(Box::new(sanitized));
    }

    // Cursor + offset mutual exclusion
    let has_cursor = query.after_cursor.is_some() || query.before_cursor.is_some();
    if has_cursor && query.offset.is_some() {
        bail!("Cannot use both cursor and offset — they are mutually exclusive");
    }
    if query.after_cursor.is_some() && query.before_cursor.is_some() {
        bail!("Cannot use both after_cursor and before_cursor — they are mutually exclusive");
    }

    // Parse sort column and direction from order_by
    // Default: created_at DESC (newest first) if timestamps enabled, else id ASC
    let (sort_col, sort_dir) = if let Some(ref order) = query.order_by {
        if let Some(stripped) = order.strip_prefix('-') {
            (stripped.to_string(), "DESC")
        } else {
            (order.clone(), "ASC")
        }
    } else if def.timestamps {
        ("created_at".to_string(), "DESC")
    } else {
        ("id".to_string(), "ASC")
    };

    // Determine active cursor (after or before) and compute keyset direction
    let active_cursor = query.after_cursor.as_ref().or(query.before_cursor.as_ref());
    let using_before = query.before_cursor.is_some();

    // Cursor keyset WHERE condition
    if let Some(cursor) = active_cursor {
        if cursor.sort_col != sort_col {
            bail!(
                "Cursor sort_col '{}' does not match query order_by '{}'",
                cursor.sort_col,
                sort_col
            );
        }
        // Forward (after_cursor): ASC → >, DESC → <
        // Backward (before_cursor): flip — ASC → <, DESC → >
        let op = match (sort_dir, using_before) {
            ("DESC", false) | ("ASC", true) => "<",
            _ => ">",
        };
        let resolved_col = resolve_filter_column(&sort_col, def, locale_ctx);
        // Keyset: (col OP ?val) OR (col = ?val AND id OP ?id)
        let keyset = format!(
            " AND (({col} {op} ?{p1}) OR ({col} = ?{p1} AND id {op} ?{p2}))",
            col = resolved_col,
            op = op,
            p1 = params.len() + 1,
            p2 = params.len() + 2,
        );
        // Convert sort_val to string for the parameter.
        // Denormalize ISO timestamps back to SQLite format for comparison:
        // "2024-01-01T12:00:00.000Z" → "2024-01-01 12:00:00"
        let sort_val_str = match &cursor.sort_val {
            serde_json::Value::String(s) => denormalize_timestamp(s),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Null => String::new(),
            other => other.to_string(),
        };
        params.push(Box::new(sort_val_str));
        params.push(Box::new(cursor.id.clone()));
        // Append to existing WHERE or start WHERE
        if where_clause.is_empty() {
            // Replace leading " AND " with " WHERE "
            sql.push_str(&keyset.replacen(" AND ", " WHERE ", 1));
        } else {
            sql.push_str(&keyset);
        }
    }

    // ORDER BY — for before_cursor, reverse the sort direction so the DB returns
    // rows in the opposite order, then we reverse them after fetching.
    let effective_dir: &str = if using_before {
        if sort_dir == "DESC" { "ASC" } else { "DESC" }
    } else if sort_dir == "DESC" {
        "DESC"
    } else {
        "ASC"
    };

    let resolved_col = resolve_filter_column(&sort_col, def, locale_ctx);
    if sort_col != "id" {
        // Stable ordering: primary sort + id tiebreaker
        sql.push_str(&format!(
            " ORDER BY {} {}, id {}",
            resolved_col, effective_dir, effective_dir
        ));
    } else {
        sql.push_str(&format!(" ORDER BY id {}", effective_dir));
    }

    if let Some(limit) = query.limit {
        params.push(Box::new(limit));
        sql.push_str(&format!(" LIMIT ?{}", params.len()));
    }
    if let Some(offset) = query.offset {
        params.push(Box::new(offset));
        sql.push_str(&format!(" OFFSET ?{}", params.len()));
    }

    let mut stmt = conn
        .prepare(&sql)
        .with_context(|| format!("Failed to prepare query: {}", sql))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt
        .query_map(params_from_iter(param_refs.iter()), |row| {
            row_to_document(row, &result_names)
        })
        .with_context(|| format!("Failed to execute query on '{}'", slug))?;

    let mut documents = Vec::new();
    for row in rows {
        let mut doc = row?;
        if let Some(ctx) = locale_ctx
            && ctx.config.is_enabled()
            && let LocaleMode::All = ctx.mode
        {
            group_locale_fields(&mut doc, &def.fields, &ctx.config);
        }
        documents.push(doc);
    }

    // before_cursor: results were fetched in reversed order, restore correct sort order
    if using_before {
        documents.reverse();
    }

    Ok(documents)
}

#[cfg(test)]
mod tests {
    use super::super::super::write::create;
    use super::super::super::{Filter, FilterClause, FilterOp, FindQuery};
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;
    use std::collections::HashMap;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
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
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn find_empty_table() {
        let conn = setup_db();
        let def = test_def();
        let query = FindQuery::default();

        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert!(docs.is_empty());
    }

    #[test]
    fn find_with_filter() {
        let conn = setup_db();
        let def = test_def();

        let mut data1 = HashMap::new();
        data1.insert("title".to_string(), "Post A".to_string());
        data1.insert("status".to_string(), "draft".to_string());
        create(&conn, "posts", &def, &data1, None).unwrap();

        let mut data2 = HashMap::new();
        data2.insert("title".to_string(), "Post B".to_string());
        data2.insert("status".to_string(), "published".to_string());
        create(&conn, "posts", &def, &data2, None).unwrap();

        let mut query = FindQuery::new();
        query.filters = vec![FilterClause::Single(Filter {
            field: "status".to_string(),
            op: FilterOp::Equals("published".to_string()),
        })];

        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].get_str("title"), Some("Post B"));
    }

    #[test]
    fn find_with_limit_offset() {
        let conn = setup_db();
        let def = test_def();

        for i in 1..=3 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Test limit
        let mut query_limit = FindQuery::new();
        query_limit.limit = Some(2);
        let docs = find(&conn, "posts", &def, &query_limit, None).unwrap();
        assert_eq!(docs.len(), 2);

        // Test limit + offset (SQLite requires LIMIT before OFFSET)
        let mut query_offset = FindQuery::new();
        query_offset.limit = Some(10);
        query_offset.offset = Some(1);
        let docs = find(&conn, "posts", &def, &query_offset, None).unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn find_with_order_by() {
        let conn = setup_db();
        let def = test_def();

        let mut data_a = HashMap::new();
        data_a.insert("title".to_string(), "Alpha".to_string());
        create(&conn, "posts", &def, &data_a, None).unwrap();

        let mut data_c = HashMap::new();
        data_c.insert("title".to_string(), "Charlie".to_string());
        create(&conn, "posts", &def, &data_c, None).unwrap();

        let mut data_b = HashMap::new();
        data_b.insert("title".to_string(), "Bravo".to_string());
        create(&conn, "posts", &def, &data_b, None).unwrap();

        // DESC order by title
        let mut query = FindQuery::new();
        query.order_by = Some("-title".to_string());
        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0].get_str("title"), Some("Charlie"));
        assert_eq!(docs[1].get_str("title"), Some("Bravo"));
        assert_eq!(docs[2].get_str("title"), Some("Alpha"));
    }

    // ── Cursor pagination tests ─────────────────────────────────────────────

    #[test]
    fn cursor_and_offset_mutual_exclusion() {
        let conn = setup_db();
        let def = test_def();

        let mut query = FindQuery::new();
        query.after_cursor = Some(super::super::super::cursor::CursorData {
            sort_col: "id".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!("abc"),
            id: "abc".to_string(),
        });
        query.offset = Some(10);
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn cursor_asc_pagination() {
        let conn = setup_db();
        let def = test_def();

        // Insert 5 rows with deterministic titles
        for i in 1..=5 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // First page: limit=2, order by title ASC
        let mut q1 = FindQuery::new();
        q1.order_by = Some("title".to_string());
        q1.limit = Some(2);
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].get_str("title"), Some("Post 01"));
        assert_eq!(page1[1].get_str("title"), Some("Post 02"));

        // Second page via cursor from last doc of page 1
        let last = &page1[1];
        let cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(last.get_str("title").unwrap()),
            id: last.id.clone(),
        };
        let mut q2 = FindQuery::new();
        q2.order_by = Some("title".to_string());
        q2.limit = Some(2);
        q2.after_cursor = Some(cursor);
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Third page
        let last2 = &page2[1];
        let cursor2 = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(last2.get_str("title").unwrap()),
            id: last2.id.clone(),
        };
        let mut q3 = FindQuery::new();
        q3.order_by = Some("title".to_string());
        q3.limit = Some(2);
        q3.after_cursor = Some(cursor2);
        let page3 = find(&conn, "posts", &def, &q3, None).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].get_str("title"), Some("Post 05"));
    }

    #[test]
    fn cursor_desc_pagination() {
        let conn = setup_db();
        let def = test_def();

        for i in 1..=4 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // First page DESC
        let mut q1 = FindQuery::new();
        q1.order_by = Some("-title".to_string());
        q1.limit = Some(2);
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Second page via cursor
        let last = &page1[1];
        let cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "DESC".to_string(),
            sort_val: serde_json::json!(last.get_str("title").unwrap()),
            id: last.id.clone(),
        };
        let mut q2 = FindQuery::new();
        q2.order_by = Some("-title".to_string());
        q2.limit = Some(2);
        q2.after_cursor = Some(cursor);
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));
    }

    #[test]
    fn cursor_wrong_sort_col_errors() {
        let conn = setup_db();
        let def = test_def();

        let mut query = FindQuery::new();
        query.order_by = Some("title".to_string());
        query.after_cursor = Some(super::super::super::cursor::CursorData {
            sort_col: "status".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!("x"),
            id: "abc".to_string(),
        });
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not match"));
    }

    #[test]
    fn before_cursor_asc_backward_pagination() {
        let conn = setup_db();
        let def = test_def();

        for i in 1..=5 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Forward: get page 2 (Posts 03, 04) so we have a cursor to go backward from
        let mut p1q = FindQuery::new();
        p1q.order_by = Some("title".to_string());
        p1q.limit = Some(2);
        let page1 = find(&conn, "posts", &def, &p1q, None).unwrap();
        let last_p1 = &page1[1];
        let fwd_cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(last_p1.get_str("title").unwrap()),
            id: last_p1.id.clone(),
        };
        let mut p2q = FindQuery::new();
        p2q.order_by = Some("title".to_string());
        p2q.limit = Some(2);
        p2q.after_cursor = Some(fwd_cursor);
        let page2 = find(&conn, "posts", &def, &p2q, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Backward: from the first doc of page 2, go backward
        let first_p2 = &page2[0];
        let back_cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(first_p2.get_str("title").unwrap()),
            id: first_p2.id.clone(),
        };
        let mut bq = FindQuery::new();
        bq.order_by = Some("title".to_string());
        bq.limit = Some(2);
        bq.before_cursor = Some(back_cursor);
        let back_page = find(&conn, "posts", &def, &bq, None).unwrap();

        // Should get Posts 01, 02 in correct ASC order
        assert_eq!(back_page.len(), 2);
        assert_eq!(back_page[0].get_str("title"), Some("Post 01"));
        assert_eq!(back_page[1].get_str("title"), Some("Post 02"));
    }

    #[test]
    fn before_cursor_desc_backward_pagination() {
        let conn = setup_db();
        let def = test_def();

        for i in 1..=4 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Forward DESC page 1: Posts 04, 03
        let mut p1q = FindQuery::new();
        p1q.order_by = Some("-title".to_string());
        p1q.limit = Some(2);
        let page1 = find(&conn, "posts", &def, &p1q, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Forward DESC page 2: Posts 02, 01
        let last_p1 = &page1[1];
        let fwd_cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "DESC".to_string(),
            sort_val: serde_json::json!(last_p1.get_str("title").unwrap()),
            id: last_p1.id.clone(),
        };
        let mut p2q = FindQuery::new();
        p2q.order_by = Some("-title".to_string());
        p2q.limit = Some(2);
        p2q.after_cursor = Some(fwd_cursor);
        let page2 = find(&conn, "posts", &def, &p2q, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));

        // Backward from page 2 first doc → should get page 1 back
        let first_p2 = &page2[0];
        let back_cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "DESC".to_string(),
            sort_val: serde_json::json!(first_p2.get_str("title").unwrap()),
            id: first_p2.id.clone(),
        };
        let mut bq = FindQuery::new();
        bq.order_by = Some("-title".to_string());
        bq.limit = Some(2);
        bq.before_cursor = Some(back_cursor);
        let back_page = find(&conn, "posts", &def, &bq, None).unwrap();

        // Should get Posts 04, 03 in DESC order
        assert_eq!(back_page.len(), 2);
        assert_eq!(back_page[0].get_str("title"), Some("Post 04"));
        assert_eq!(back_page[1].get_str("title"), Some("Post 03"));
    }

    #[test]
    fn after_and_before_cursor_mutual_exclusion() {
        let conn = setup_db();
        let def = test_def();

        let cursor = super::super::super::cursor::CursorData {
            sort_col: "id".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!("abc"),
            id: "abc".to_string(),
        };
        let mut query = FindQuery::new();
        query.after_cursor = Some(cursor.clone());
        query.before_cursor = Some(cursor);
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("mutually exclusive")
        );
    }

    #[test]
    fn cursor_sort_val_number_in_params() {
        // Inserting docs and using a numeric sort_val cursor
        let conn = setup_db();
        let def = test_def();

        for i in 1..=3 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Build a cursor with a Number sort_val (e.g. for a numeric field)
        // We use "id" as sort_col and supply a numeric sort_val to exercise the Number arm
        let mut q = FindQuery::new();
        q.order_by = Some("title".to_string());
        q.limit = Some(1);
        let page1 = find(&conn, "posts", &def, &q, None).unwrap();
        assert_eq!(page1.len(), 1);

        // Build cursor with Number sort_val
        let cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(99i64), // Number variant
            id: page1[0].id.clone(),
        };
        let mut q2 = FindQuery::new();
        q2.order_by = Some("title".to_string());
        q2.limit = Some(10);
        q2.after_cursor = Some(cursor);
        // Should execute without error (all docs have title > 99 as string comparison goes)
        let result = find(&conn, "posts", &def, &q2, None);
        assert!(result.is_ok());
    }

    #[test]
    fn cursor_sort_val_bool_in_params() {
        let conn = setup_db();
        let def = test_def();

        // Bool variant exercises the `other => other.to_string()` arm
        let cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(true), // Bool variant
            id: "anyid".to_string(),
        };
        let mut q = FindQuery::new();
        q.order_by = Some("title".to_string());
        q.after_cursor = Some(cursor);
        let result = find(&conn, "posts", &def, &q, None);
        assert!(result.is_ok());
    }

    #[test]
    fn cursor_appended_to_existing_where_clause() {
        let conn = setup_db();
        let def = test_def();

        // Insert some docs
        for i in 1..=3 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            data.insert("status".to_string(), "active".to_string());
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Use a filter (creates WHERE) plus cursor (appends AND condition).
        // Anchor id must sort after all nanoid chars ('~' = ASCII 126 > 'z' = 122)
        // so the tie-break condition `id > anchor` is always false for Post 01,
        // guaranteeing only strictly-after-title results are returned.
        let cursor = super::super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!("Post 01"),
            id: "~".to_string(),
        };
        let mut q = FindQuery::new();
        q.order_by = Some("title".to_string());
        q.filters = vec![FilterClause::Single(Filter {
            field: "status".to_string(),
            op: FilterOp::Equals("active".to_string()),
        })];
        q.after_cursor = Some(cursor);
        let result = find(&conn, "posts", &def, &q, None).unwrap();
        // All posts have status=active, but cursor anchors after "Post 01"
        assert!(
            result
                .iter()
                .all(|d| d.get_str("title").unwrap_or("") > "Post 01")
        );
    }

    #[test]
    fn find_default_sort_without_timestamps() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE items (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();

        let mut def = CollectionDefinition::new("items");
        def.timestamps = false; // No timestamps
        def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let def = def;

        conn.execute("INSERT INTO items (id, name) VALUES ('b', 'Banana')", [])
            .unwrap();
        conn.execute("INSERT INTO items (id, name) VALUES ('a', 'Apple')", [])
            .unwrap();

        // Default sort for no-timestamp collection is id ASC
        let q = FindQuery::default();
        let docs = find(&conn, "items", &def, &q, None).unwrap();
        assert_eq!(docs.len(), 2);
        // id ASC: 'a' before 'b'
        assert_eq!(docs[0].id, "a");
        assert_eq!(docs[1].id, "b");
    }

    #[test]
    fn find_order_by_id_uses_single_order_clause() {
        let conn = setup_db();
        let def = test_def();

        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "A".to_string());
        create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = HashMap::new();
        d2.insert("title".to_string(), "B".to_string());
        create(&conn, "posts", &def, &d2, None).unwrap();

        // Sorting by "id" should use single ORDER BY clause (not the tiebreaker form)
        let mut q = FindQuery::new();
        q.order_by = Some("id".to_string());
        let docs = find(&conn, "posts", &def, &q, None).unwrap();
        assert_eq!(docs.len(), 2);
        // Just verify it executes successfully — order is nanoid-determined
    }

    // ── denormalize_timestamp tests ──────────────────────────────────────

    #[test]
    fn denormalize_timestamp_iso_to_sqlite() {
        assert_eq!(
            denormalize_timestamp("2026-03-01T19:13:04.000Z"),
            "2026-03-01 19:13:04"
        );
    }

    #[test]
    fn denormalize_timestamp_passthrough() {
        // Already in SQLite format — unchanged
        assert_eq!(
            denormalize_timestamp("2026-03-01 19:13:04"),
            "2026-03-01 19:13:04"
        );
        // Non-timestamp string — unchanged
        assert_eq!(denormalize_timestamp("hello"), "hello");
    }

    #[test]
    fn denormalize_timestamp_wrong_length_passthrough() {
        // String has T at index 10 but wrong length — passthrough unchanged
        assert_eq!(
            denormalize_timestamp("2026-03-01T12:00:00"), // len 19, not 24
            "2026-03-01T12:00:00"
        );
    }

    #[test]
    fn denormalize_timestamp_no_000z_suffix_passthrough() {
        // Correct length but does not end in ".000Z"
        let s = "2026-03-01T12:00:00.999Z"; // ends in .999Z not .000Z
        assert_eq!(denormalize_timestamp(s), s);
    }
}
