//! `find()` — query multiple documents with filters, sorting, and cursor pagination.

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use super::select::apply_select_filter;
use crate::{
    core::{CollectionDefinition, Document, FieldDefinition, FieldType},
    db::{
        DbConnection, DbRow, DbValue, FindQuery, LocaleContext, LocaleMode,
        document::row_to_document,
        query::{
            cursor::{CursorData, SortDirection},
            filter::{build_where_clause, resolve_filter_column, resolve_filters},
            fts, get_column_names, get_locale_select_columns_full, group_locale_fields,
            helpers::{append_sql_condition, prefixed_name},
            resolve_sort as resolve_order, validate_query_fields,
        },
    },
};

/// Find documents matching a query.
pub fn find(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<Document>> {
    validate_query_fields(def, query, locale_ctx)?;

    let select_exprs = build_select(def, query, locale_ctx)?;
    let mut sql = format!("SELECT {} FROM \"{slug}\"", select_exprs.join(", "));
    let mut params: Vec<DbValue> = Vec::new();
    let mut has_where = false;

    let resolved_filters = resolve_filters(&query.filters, def, locale_ctx)?;
    let where_clause = build_where_clause(
        conn,
        &resolved_filters,
        slug,
        &def.fields,
        locale_ctx,
        &mut params,
    )?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
        has_where = true;
    }

    apply_fts(conn, slug, query, &mut sql, &mut has_where, &mut params);
    apply_soft_delete(def, query, &mut sql, &mut has_where);

    let (sort_col, sort_dir, using_before) = resolve_sort(def, query)?;

    if let Some(cursor) = query.after_cursor.as_ref().or(query.before_cursor.as_ref()) {
        let sort = SortInfo {
            col: &sort_col,
            dir: sort_dir,
            using_before,
        };

        let resolved = resolve_filter_column(&sort_col, def, locale_ctx)?;

        apply_cursor_keyset(
            conn,
            cursor,
            &sort,
            &resolved,
            &mut sql,
            &mut has_where,
            &mut params,
        )?;
    }
    apply_order_by(&sort_col, sort_dir, using_before, def, locale_ctx, &mut sql)?;
    apply_limit_offset(conn, query, &mut sql, &mut params);

    let rows = conn
        .query_all(&sql, &params)
        .with_context(|| format!("Failed to execute query on '{slug}'"))?;

    map_rows(conn, &rows, locale_ctx, def, using_before)
}

/// Build the SELECT column list.
fn build_select(
    def: &CollectionDefinition,
    query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<String>> {
    let (select_exprs, result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => get_locale_select_columns_full(
            &def.fields,
            def.timestamps,
            def.soft_delete,
            def.has_drafts(),
            ctx,
        )?,
        _ => {
            let names = get_column_names(def);
            (names.clone(), names)
        }
    };

    let (select_exprs, _) = apply_select_filter(select_exprs, result_names, query.select.as_ref());

    Ok(select_exprs)
}

/// Apply FTS search filter if present.
fn apply_fts(
    conn: &dyn DbConnection,
    slug: &str,
    query: &FindQuery,
    sql: &mut String,
    has_where: &mut bool,
    params: &mut Vec<DbValue>,
) {
    if let Some((clause, sanitized)) = query
        .search
        .as_deref()
        .and_then(|term| fts::fts_where_clause(conn, slug, term, params.len() + 1))
    {
        append_sql_condition(sql, has_where, &clause);
        params.push(DbValue::Text(sanitized));
    }
}

/// Exclude soft-deleted documents unless explicitly requested.
fn apply_soft_delete(
    def: &CollectionDefinition,
    query: &FindQuery,
    sql: &mut String,
    has_where: &mut bool,
) {
    if def.soft_delete && !query.include_deleted {
        append_sql_condition(sql, has_where, "_deleted_at IS NULL");
    }
}

/// Resolve sort column, direction, and cursor mode from query.
fn resolve_sort(
    def: &CollectionDefinition,
    query: &FindQuery,
) -> Result<(String, SortDirection, bool)> {
    let has_cursor = query.after_cursor.is_some() || query.before_cursor.is_some();

    if has_cursor && query.offset.is_some() {
        bail!("Cannot use both cursor and offset — they are mutually exclusive");
    }

    if query.after_cursor.is_some() && query.before_cursor.is_some() {
        bail!("Cannot use both after_cursor and before_cursor — they are mutually exclusive");
    }

    let (sort_col, sort_dir) = resolve_order(query.order_by.as_deref(), def.timestamps);

    if !is_valid_sort_column(&sort_col, def) {
        bail!(
            "Invalid sort column '{}' — not a column on '{}'",
            sort_col,
            def.slug
        );
    }

    Ok((sort_col, sort_dir, query.before_cursor.is_some()))
}

/// Append ORDER BY clause with stable tiebreaker.
fn apply_order_by(
    sort_col: &str,
    sort_dir: SortDirection,
    using_before: bool,
    def: &CollectionDefinition,
    locale_ctx: Option<&LocaleContext>,
    sql: &mut String,
) -> Result<()> {
    let effective_dir = if using_before {
        sort_dir.flip()
    } else {
        sort_dir
    };
    let resolved = resolve_filter_column(sort_col, def, locale_ctx)?;

    if sort_col != "id" {
        sql.push_str(&format!(
            " ORDER BY {resolved} {effective_dir}, id {effective_dir}"
        ));
    } else {
        sql.push_str(&format!(" ORDER BY id {effective_dir}"));
    }

    Ok(())
}

/// Append LIMIT and OFFSET clauses.
fn apply_limit_offset(
    conn: &dyn DbConnection,
    query: &FindQuery,
    sql: &mut String,
    params: &mut Vec<DbValue>,
) {
    if let Some(limit) = query.limit {
        let ph = conn.placeholder(params.len() + 1);
        params.push(DbValue::Integer(limit.max(0)));
        sql.push_str(&format!(" LIMIT {ph}"));
    }

    if let Some(offset) = query.offset {
        let ph = conn.placeholder(params.len() + 1);
        params.push(DbValue::Integer(offset.max(0)));
        sql.push_str(&format!(" OFFSET {ph}"));
    }
}

/// Execute the query and map rows to documents.
fn map_rows(
    conn: &dyn DbConnection,
    rows: &[DbRow],
    locale_ctx: Option<&LocaleContext>,
    def: &CollectionDefinition,
    using_before: bool,
) -> Result<Vec<Document>> {
    let mut documents = Vec::new();

    for row in rows {
        let mut doc = row_to_document(conn, row)?;

        if let Some(ctx) = locale_ctx
            && ctx.config.is_enabled()
            && let LocaleMode::All = ctx.mode
        {
            group_locale_fields(&mut doc, &def.fields, &ctx.config)?;
        }

        documents.push(doc);
    }

    if using_before {
        documents.reverse();
    }

    Ok(documents)
}

/// Check whether a sort column name corresponds to a real column on the collection table.
fn is_valid_sort_column(col: &str, def: &CollectionDefinition) -> bool {
    // System columns that always exist
    if matches!(
        col,
        "id" | "created_at" | "updated_at" | "_status" | "_deleted_at" | "_ref_count"
    ) {
        return true;
    }

    // User-defined fields that have a parent column (has-one scalar fields).
    // Layout wrappers (Row, Collapsible, Tabs) promote their children to
    // parent-level columns, so we recurse into them.
    // Group sub-fields use `group__subfield` naming for DB columns.
    fn check_fields(col: &str, fields: &[FieldDefinition], prefix: &str) -> bool {
        fields.iter().any(|f| {
            let full_name = prefixed_name(prefix, &f.name);

            if full_name == col && f.has_parent_column() {
                return true;
            }

            match f.field_type {
                FieldType::Group => check_fields(col, &f.fields, &full_name),
                FieldType::Row | FieldType::Collapsible => check_fields(col, &f.fields, prefix),
                FieldType::Tabs => f
                    .tabs
                    .iter()
                    .any(|tab| check_fields(col, &tab.fields, prefix)),
                _ => false,
            }
        })
    }

    check_fields(col, &def.fields, "")
}

/// Convert a JSON value to its DbValue representation for cursor comparison.
fn cursor_sort_value(val: &Value) -> DbValue {
    match val {
        Value::String(s) => DbValue::Text(s.clone()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                DbValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                DbValue::Real(f)
            } else {
                DbValue::Text(n.to_string())
            }
        }
        Value::Null => DbValue::Null,
        other => DbValue::Text(other.to_string()),
    }
}

/// Resolved sort configuration for cursor pagination.
struct SortInfo<'a> {
    col: &'a str,
    dir: SortDirection,
    using_before: bool,
}

/// Apply cursor-based keyset pagination to the SQL query.
fn apply_cursor_keyset(
    conn: &dyn DbConnection,
    cursor: &CursorData,
    sort: &SortInfo<'_>,
    resolved_col: &str,
    sql: &mut String,
    has_where: &mut bool,
    params: &mut Vec<DbValue>,
) -> Result<()> {
    if cursor.sort_col != sort.col {
        bail!(
            "Cursor sort_col '{}' does not match query order_by '{}'",
            cursor.sort_col,
            sort.col
        );
    }

    let op = match (sort.dir, sort.using_before) {
        (SortDirection::Desc, false) | (SortDirection::Asc, true) => "<",
        _ => ">",
    };
    let sort_val = cursor_sort_value(&cursor.sort_val);

    let keyset = if matches!(sort_val, DbValue::Null) {
        build_null_keyset(conn, resolved_col, op, &cursor.id, params)
    } else {
        build_standard_keyset(conn, resolved_col, op, sort_val, &cursor.id, params)
    };

    if *has_where {
        sql.push_str(&keyset);
    } else {
        sql.push_str(&keyset.replacen(" AND ", " WHERE ", 1));
    }

    Ok(())
}

/// Build keyset condition for NULL sort values.
fn build_null_keyset(
    conn: &dyn DbConnection,
    col: &str,
    op: &str,
    cursor_id: &str,
    params: &mut Vec<DbValue>,
) -> String {
    let ph_id = conn.placeholder(params.len() + 1);
    params.push(DbValue::Text(cursor_id.to_string()));

    if op == ">" {
        format!(" AND (({col} IS NULL AND id > {ph_id}) OR {col} IS NOT NULL)")
    } else {
        format!(" AND ({col} IS NULL AND id < {ph_id})")
    }
}

/// Build standard keyset condition for non-NULL sort values.
fn build_standard_keyset(
    conn: &dyn DbConnection,
    col: &str,
    op: &str,
    sort_val: DbValue,
    cursor_id: &str,
    params: &mut Vec<DbValue>,
) -> String {
    let ph1 = conn.placeholder(params.len() + 1);
    let ph2 = conn.placeholder(params.len() + 2);
    params.push(sort_val);
    params.push(DbValue::Text(cursor_id.to_string()));

    format!(" AND (({col} {op} {ph1}) OR ({col} = {ph1} AND id {op} {ph2}))")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig};
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{
        DbPool, Filter, FilterClause, FilterOp, FindQuery, pool,
        query::{cursor::build_cursors, write::create},
    };
    use std::collections::HashMap;
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
    fn find_empty_table() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();
        let query = FindQuery::default();

        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert!(docs.is_empty());
    }

    #[test]
    fn find_with_filter() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut data1 = HashMap::new();
        data1.insert("title".to_string(), "Post A".to_string());
        data1.insert("status".to_string(), "draft".to_string());
        create(&conn, "posts", &def, &data1, None).unwrap();

        let mut data2 = HashMap::new();
        data2.insert("title".to_string(), "Post B".to_string());
        data2.insert("status".to_string(), "published".to_string());
        create(&conn, "posts", &def, &data2, None).unwrap();

        let query = FindQuery::builder()
            .filters(vec![FilterClause::Single(Filter {
                field: "status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            })])
            .build();

        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].get_str("title"), Some("Post B"));
    }

    #[test]
    fn find_with_limit_offset() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        for i in 1..=3 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {i}"));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Test limit
        let query_limit = FindQuery::builder().limit(Some(2)).build();
        let docs = find(&conn, "posts", &def, &query_limit, None).unwrap();
        assert_eq!(docs.len(), 2);

        // Test limit + offset (SQLite requires LIMIT before OFFSET)
        let query_offset = FindQuery::builder().limit(Some(10)).offset(Some(1)).build();
        let docs = find(&conn, "posts", &def, &query_offset, None).unwrap();
        assert_eq!(docs.len(), 2);
    }

    #[test]
    fn find_with_order_by() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
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
        let query = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .build();
        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0].get_str("title"), Some("Charlie"));
        assert_eq!(docs[1].get_str("title"), Some("Bravo"));
        assert_eq!(docs[2].get_str("title"), Some("Alpha"));
    }

    // ── Cursor pagination tests ─────────────────────────────────────────────

    #[test]
    fn cursor_and_offset_mutual_exclusion() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .after_cursor(Some(CursorData {
                sort_col: "id".to_string(),
                sort_dir: SortDirection::Asc,
                sort_val: json!("abc"),
                id: "abc".to_string(),
            }))
            .offset(Some(10))
            .build();
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
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Insert 5 rows with deterministic titles
        for i in 1..=5 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // First page: limit=2, order by title ASC
        let q1 = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].get_str("title"), Some("Post 01"));
        assert_eq!(page1[1].get_str("title"), Some("Post 02"));

        // Second page via cursor from last doc of page 1
        let last = &page1[1];
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(last.get_str("title").unwrap()),
            id: last.id.to_string(),
        };
        let q2 = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Third page
        let last2 = &page2[1];
        let cursor2 = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(last2.get_str("title").unwrap()),
            id: last2.id.to_string(),
        };
        let q3 = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor2))
            .build();
        let page3 = find(&conn, "posts", &def, &q3, None).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].get_str("title"), Some("Post 05"));
    }

    #[test]
    fn cursor_desc_pagination() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        for i in 1..=4 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // First page DESC
        let q1 = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Second page via cursor
        let last = &page1[1];
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: json!(last.get_str("title").unwrap()),
            id: last.id.to_string(),
        };
        let q2 = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));
    }

    #[test]
    fn cursor_wrong_sort_col_errors() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(CursorData {
                sort_col: "status".to_string(),
                sort_dir: SortDirection::Asc,
                sort_val: json!("x"),
                id: "abc".to_string(),
            }))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not match"));
    }

    #[test]
    fn before_cursor_asc_backward_pagination() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        for i in 1..=5 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Forward: get page 2 (Posts 03, 04) so we have a cursor to go backward from
        let p1q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &p1q, None).unwrap();
        let last_p1 = &page1[1];
        let fwd_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(last_p1.get_str("title").unwrap()),
            id: last_p1.id.to_string(),
        };
        let p2q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(fwd_cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &p2q, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Backward: from the first doc of page 2, go backward
        let first_p2 = &page2[0];
        let back_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(first_p2.get_str("title").unwrap()),
            id: first_p2.id.to_string(),
        };
        let bq = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .limit(Some(2))
            .before_cursor(Some(back_cursor))
            .build();
        let back_page = find(&conn, "posts", &def, &bq, None).unwrap();

        // Should get Posts 01, 02 in correct ASC order
        assert_eq!(back_page.len(), 2);
        assert_eq!(back_page[0].get_str("title"), Some("Post 01"));
        assert_eq!(back_page[1].get_str("title"), Some("Post 02"));
    }

    #[test]
    fn before_cursor_desc_backward_pagination() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        for i in 1..=4 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            create(&conn, "posts", &def, &data, None).unwrap();
        }

        // Forward DESC page 1: Posts 04, 03
        let p1q = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &p1q, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Forward DESC page 2: Posts 02, 01
        let last_p1 = &page1[1];
        let fwd_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: json!(last_p1.get_str("title").unwrap()),
            id: last_p1.id.to_string(),
        };
        let p2q = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .after_cursor(Some(fwd_cursor))
            .build();
        let page2 = find(&conn, "posts", &def, &p2q, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));

        // Backward from page 2 first doc → should get page 1 back
        let first_p2 = &page2[0];
        let back_cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: json!(first_p2.get_str("title").unwrap()),
            id: first_p2.id.to_string(),
        };
        let bq = FindQuery::builder()
            .order_by(Some("-title".to_string()))
            .limit(Some(2))
            .before_cursor(Some(back_cursor))
            .build();
        let back_page = find(&conn, "posts", &def, &bq, None).unwrap();

        // Should get Posts 04, 03 in DESC order
        assert_eq!(back_page.len(), 2);
        assert_eq!(back_page[0].get_str("title"), Some("Post 04"));
        assert_eq!(back_page[1].get_str("title"), Some("Post 03"));
    }

    #[test]
    fn after_and_before_cursor_mutual_exclusion() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let cursor = CursorData {
            sort_col: "id".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!("abc"),
            id: "abc".to_string(),
        };
        let query = FindQuery::builder()
            .after_cursor(Some(cursor.clone()))
            .before_cursor(Some(cursor))
            .build();
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
        // Numeric cursor pagination must use numeric comparison, not string.
        // With string comparison "9" > "10", so pagination would be wrong.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let conn = db_pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE scores (
                id TEXT PRIMARY KEY,
                name TEXT,
                points INTEGER,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("scores");
        def.fields = vec![
            FieldDefinition::builder("name", FieldType::Text).build(),
            FieldDefinition::builder("points", FieldType::Number).build(),
        ];

        // Insert rows with numeric values that would sort wrong as strings
        // String order: "10" < "5" < "9" (lexicographic)
        // Numeric order: 5 < 9 < 10 < 20 < 100
        let values = [
            (5, "five"),
            (9, "nine"),
            (10, "ten"),
            (20, "twenty"),
            (100, "hundred"),
        ];

        for (pts, name) in &values {
            conn.execute(
                "INSERT INTO scores (id, name, points, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                &[
                    DbValue::Text(format!("id-{name}")),
                    DbValue::Text(name.to_string()),
                    DbValue::Integer(*pts),
                    DbValue::Text("2026-01-01 00:00:00".into()),
                ],
            )
            .unwrap();
        }

        // Page 1: limit 2, order by points ASC → should get 5, 9
        let q1 = FindQuery::builder()
            .order_by(Some("points".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "scores", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].get_str("name"), Some("five"));
        assert_eq!(page1[1].get_str("name"), Some("nine"));

        // Page 2: cursor after points=9 → should get 10, 20 (NOT skip 10 as string "9" > "10")
        let cursor = CursorData {
            sort_col: "points".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(9),
            id: "id-nine".to_string(),
        };
        let q2 = FindQuery::builder()
            .order_by(Some("points".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor))
            .build();
        let page2 = find(&conn, "scores", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("name"), Some("ten"));
        assert_eq!(page2[1].get_str("name"), Some("twenty"));

        // Page 3: cursor after points=20 → should get 100
        let cursor2 = CursorData {
            sort_col: "points".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(20),
            id: "id-twenty".to_string(),
        };
        let q3 = FindQuery::builder()
            .order_by(Some("points".to_string()))
            .limit(Some(2))
            .after_cursor(Some(cursor2))
            .build();
        let page3 = find(&conn, "scores", &def, &q3, None).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].get_str("name"), Some("hundred"));
    }

    #[test]
    fn cursor_sort_val_null_binds_as_null() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Null sort_val should execute without error (binds DbValue::Null, not empty string)
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: Value::Null,
            id: "anyid".to_string(),
        };
        let q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(cursor))
            .build();
        let result = find(&conn, "posts", &def, &q, None);
        assert!(result.is_ok());
    }

    #[test]
    fn cursor_sort_val_real_in_params() {
        // Verify f64 cursor values bind as DbValue::Real
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let conn = db_pool.get().unwrap();

        conn.execute_batch(
            "CREATE TABLE ratings (
                id TEXT PRIMARY KEY,
                label TEXT,
                score REAL,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("ratings");
        def.fields = vec![
            FieldDefinition::builder("label", FieldType::Text).build(),
            FieldDefinition::builder("score", FieldType::Number).build(),
        ];

        let values = [(1.5, "low"), (2.7, "mid"), (3.9, "high")];

        for (score, label) in &values {
            conn.execute(
                "INSERT INTO ratings (id, label, score, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                &[
                    DbValue::Text(format!("id-{label}")),
                    DbValue::Text(label.to_string()),
                    DbValue::Real(*score),
                    DbValue::Text("2026-01-01 00:00:00".into()),
                ],
            )
            .unwrap();
        }

        // Cursor after score=1.5 → should get mid, high
        let cursor = CursorData {
            sort_col: "score".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(1.5),
            id: "id-low".to_string(),
        };
        let q = FindQuery::builder()
            .order_by(Some("score".to_string()))
            .limit(Some(10))
            .after_cursor(Some(cursor))
            .build();
        let results = find(&conn, "ratings", &def, &q, None).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].get_str("label"), Some("mid"));
        assert_eq!(results[1].get_str("label"), Some("high"));
    }

    #[test]
    fn cursor_sort_val_bool_in_params() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Bool variant exercises the `other => other.to_string()` arm
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!(true), // Bool variant
            id: "anyid".to_string(),
        };
        let q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(cursor))
            .build();
        let result = find(&conn, "posts", &def, &q, None);
        assert!(result.is_ok());
    }

    #[test]
    fn cursor_appended_to_existing_where_clause() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
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
        let cursor = CursorData {
            sort_col: "title".to_string(),
            sort_dir: SortDirection::Asc,
            sort_val: json!("Post 01"),
            id: "~".to_string(),
        };
        let q = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .filters(vec![FilterClause::Single(Filter {
                field: "status".to_string(),
                op: FilterOp::Equals("active".to_string()),
            })])
            .after_cursor(Some(cursor))
            .build();
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
        let tmp = tempfile::tempdir().expect("tempdir");
        let config = CrapConfig {
            database: DatabaseConfig {
                path: "test.db".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let db_pool = pool::create_pool(tmp.path(), &config).expect("pool");
        let conn = db_pool.get().unwrap();
        conn.execute_batch("CREATE TABLE items (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();

        let mut def = CollectionDefinition::new("items");
        def.timestamps = false; // No timestamps
        def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let def = def;

        conn.execute(
            "INSERT INTO items (id, name) VALUES (?1, ?2)",
            &[DbValue::Text("b".into()), DbValue::Text("Banana".into())],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO items (id, name) VALUES (?1, ?2)",
            &[DbValue::Text("a".into()), DbValue::Text("Apple".into())],
        )
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
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "A".to_string());
        create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = HashMap::new();
        d2.insert("title".to_string(), "B".to_string());
        create(&conn, "posts", &def, &d2, None).unwrap();

        // Sorting by "id" should use single ORDER BY clause (not the tiebreaker form)
        let q = FindQuery::builder()
            .order_by(Some("id".to_string()))
            .build();
        let docs = find(&conn, "posts", &def, &q, None).unwrap();
        assert_eq!(docs.len(), 2);
        // Just verify it executes successfully — order is nanoid-determined
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
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
        def.soft_delete = true;
        def
    }

    #[test]
    fn find_excludes_soft_deleted_by_default() {
        let (_tmp, pool) = setup_soft_delete_db();
        let conn = pool.get().unwrap();
        let def = soft_delete_def();

        // Insert a normal doc and a soft-deleted doc
        conn.execute(
            "INSERT INTO articles (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            &[
                DbValue::Text("id-live".into()),
                DbValue::Text("Live Post".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO articles (id, title, _deleted_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            &[
                DbValue::Text("id-deleted".into()),
                DbValue::Text("Deleted Post".into()),
                DbValue::Text("2026-01-02 00:00:00".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        let query = FindQuery::default();
        let docs = find(&conn, "articles", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].get_str("title"), Some("Live Post"));
    }

    #[test]
    fn find_includes_soft_deleted_when_requested() {
        let (_tmp, pool) = setup_soft_delete_db();
        let conn = pool.get().unwrap();
        let def = soft_delete_def();

        conn.execute(
            "INSERT INTO articles (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            &[
                DbValue::Text("id-live".into()),
                DbValue::Text("Live Post".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO articles (id, title, _deleted_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            &[
                DbValue::Text("id-deleted".into()),
                DbValue::Text("Deleted Post".into()),
                DbValue::Text("2026-01-02 00:00:00".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        let query = FindQuery::builder().include_deleted(true).build();
        let docs = find(&conn, "articles", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 2);
    }

    // ── Invalid sort column ──────────────────────────────────────────────

    #[test]
    fn invalid_sort_column_returns_error_not_500() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .order_by(Some("nonexistent_column".to_string()))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err(), "Should reject invalid sort column");
        // Caught by validate_query_fields before reaching SQL
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid field"),
            "Should be a validation error, got: {err_msg}"
        );
    }

    #[test]
    fn valid_cursor_sort_col_succeeds() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let query = FindQuery::builder()
            .order_by(Some("title".to_string()))
            .after_cursor(Some(CursorData {
                sort_col: "title".to_string(),
                sort_dir: SortDirection::Asc,
                sort_val: Value::String("test".to_string()),
                id: "abc".to_string(),
            }))
            .build();
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_ok());
    }

    // ── Cursor pagination round-trip consistency ─────────────────────────

    #[test]
    fn cursor_forward_back_forward_consistent() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Insert 14 docs with unique sequential created_at (ISO format, matching DB storage)
        for i in 1..=14 {
            conn.execute(
                &format!(
                    "INSERT INTO posts (id, title, created_at, updated_at) VALUES ('d{:02}', 'Post {}', '2024-01-{:02}T12:00:00.000Z', '2024-01-{:02}T12:00:00.000Z')",
                    i, i, i, i
                ),
                &[],
            ).unwrap();
        }

        let limit = 10i64;

        // Page 1: initial load (no cursor, limit=10, default sort: -created_at)
        let q1 = FindQuery::builder().limit(Some(limit)).build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 10, "Page 1 should have 10 items");
        // DESC: newest first, so d14, d13, ..., d05
        assert_eq!(page1[0].id.as_ref(), "d14");
        assert_eq!(page1[9].id.as_ref(), "d05");

        // Page 2: forward with after_cursor (overfetch limit=11)
        let (_, end_cursor_p1) = build_cursors(&page1, "created_at", SortDirection::Desc);
        let end_cursor_data = CursorData::decode(end_cursor_p1.as_ref().unwrap()).unwrap();
        let q2 = FindQuery::builder()
            .limit(Some(limit + 1))
            .after_cursor(Some(end_cursor_data))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        let page2_count = page2.len().min(limit as usize);
        assert_eq!(page2_count, 4, "Page 2 should have 4 items");
        assert_eq!(page2[0].id.as_ref(), "d04");

        // Grab the start_cursor of page 2 for going back
        let page2_trimmed = &page2[..page2_count];
        let (start_cursor_p2, _) = build_cursors(page2_trimmed, "created_at", SortDirection::Desc);
        let start_cursor_data = CursorData::decode(start_cursor_p2.as_ref().unwrap()).unwrap();

        // Go back: before_cursor (overfetch limit=11)
        let q_back = FindQuery::builder()
            .limit(Some(limit + 1))
            .before_cursor(Some(start_cursor_data))
            .build();
        let page1_again = find(&conn, "posts", &def, &q_back, None).unwrap();
        // Trim overfetch from front (before_cursor extra is at index 0 after reversal)
        let page1_trimmed: Vec<_> = if page1_again.len() > limit as usize {
            page1_again[1..].to_vec()
        } else {
            page1_again
        };
        assert_eq!(
            page1_trimmed.len(),
            10,
            "Back to page 1 should have 10 items"
        );
        assert_eq!(
            page1_trimmed[0].id.as_ref(),
            "d14",
            "First item should be d14"
        );
        assert_eq!(
            page1_trimmed[9].id.as_ref(),
            "d05",
            "Last item should be d05"
        );

        // Forward again: end_cursor of the back-result
        let (_, end_cursor_p1_again) =
            build_cursors(&page1_trimmed, "created_at", SortDirection::Desc);
        let end_cursor_data_again =
            CursorData::decode(end_cursor_p1_again.as_ref().unwrap()).unwrap();
        let q2_again = FindQuery::builder()
            .limit(Some(limit + 1))
            .after_cursor(Some(end_cursor_data_again))
            .build();
        let page2_again = find(&conn, "posts", &def, &q2_again, None).unwrap();
        let page2_again_count = page2_again.len().min(limit as usize);
        assert_eq!(
            page2_again_count, page2_count,
            "Page 2 after back+forward should have same item count"
        );

        // Verify same IDs
        let ids_first: Vec<&str> = page2_trimmed.iter().map(|d| d.id.as_ref()).collect();
        let ids_second: Vec<&str> = page2_again[..page2_again_count]
            .iter()
            .map(|d| d.id.as_ref())
            .collect();
        assert_eq!(
            ids_first, ids_second,
            "Same documents should appear on page 2"
        );
    }

    // ── Regression: is_valid_sort_column with layout wrappers ─────────

    #[test]
    fn sort_column_inside_row_is_valid() {
        let mut def = CollectionDefinition::new("events");
        def.fields = vec![FieldDefinition {
            name: "date_row".to_string(),
            field_type: FieldType::Row,
            fields: vec![FieldDefinition {
                name: "start_date".to_string(),
                field_type: FieldType::Date,
                ..Default::default()
            }],
            ..Default::default()
        }];

        assert!(
            is_valid_sort_column("start_date", &def),
            "Field inside Row should be valid sort column"
        );
    }

    #[test]
    fn sort_column_inside_collapsible_is_valid() {
        let mut def = CollectionDefinition::new("items");
        def.fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "priority".to_string(),
                field_type: FieldType::Number,
                ..Default::default()
            }],
            ..Default::default()
        }];

        assert!(
            is_valid_sort_column("priority", &def),
            "Field inside Collapsible should be valid sort column"
        );
    }

    #[test]
    fn sort_column_inside_tabs_is_valid() {
        use crate::core::field::FieldTab;

        let mut def = CollectionDefinition::new("pages");
        def.fields = vec![
            FieldDefinition::builder("content_tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Main",
                    vec![FieldDefinition::builder("title", FieldType::Text).build()],
                )])
                .build(),
        ];

        assert!(
            is_valid_sort_column("title", &def),
            "Field inside Tabs should be valid sort column"
        );
    }

    #[test]
    fn sort_column_nonexistent_is_invalid() {
        let def = test_def();
        assert!(
            !is_valid_sort_column("nonexistent", &def),
            "Nonexistent field should be invalid sort column"
        );
    }

    #[test]
    fn sort_column_group_sub_field_is_valid() {
        let mut def = CollectionDefinition::new("pages");
        def.fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        assert!(
            is_valid_sort_column("seo__title", &def),
            "Group sub-field should be valid sort column with __ prefix"
        );
        assert!(
            !is_valid_sort_column("title", &def),
            "Bare sub-field name should not be valid without group prefix"
        );
    }

    #[test]
    fn sort_column_group_in_tabs_is_valid() {
        use crate::core::field::FieldTab;

        let mut def = CollectionDefinition::new("pages");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "SEO",
                    vec![
                        FieldDefinition::builder("seo", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("title", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];

        assert!(
            is_valid_sort_column("seo__title", &def),
            "Group sub-field inside Tabs should be valid sort column"
        );
    }

    /// Regression: negative limit/offset must be clamped to 0 instead of
    /// passing undefined values to SQLite.
    #[test]
    fn negative_limit_and_offset_clamped_to_zero() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut data = HashMap::new();
        data.insert("title".to_string(), "A".to_string());
        create(&conn, "posts", &def, &data, None).unwrap();

        let mut data2 = HashMap::new();
        data2.insert("title".to_string(), "B".to_string());
        create(&conn, "posts", &def, &data2, None).unwrap();

        let query = FindQuery::builder()
            .limit(Some(-5))
            .offset(Some(-10))
            .build();

        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        // Negative limit clamped to 0 → returns zero rows
        assert!(
            docs.is_empty(),
            "Negative limit should be clamped to 0, returning no rows"
        );
    }
}
