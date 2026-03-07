//! Read operations: find, find_by_id, count, select filtering.

use anyhow::{Context, Result, bail};
use rusqlite::params_from_iter;
use std::collections::HashSet;

use crate::core::{CollectionDefinition, Document};
use crate::core::field::FieldType;
use crate::db::document::row_to_document;
use super::{
    LocaleMode, LocaleContext, FindQuery, FilterClause,
    get_column_names, get_locale_select_columns,
    validate_query_fields, group_locale_fields,
    is_valid_identifier,
};
use super::filter::{build_where_clause, resolve_filters, resolve_filter_column};

/// Convert ISO 8601 timestamp back to SQLite storage format for cursor comparison.
/// "2024-01-01T12:00:00.000Z" → "2024-01-01 12:00:00"
fn denormalize_timestamp(s: &str) -> String {
    // Match the ISO format produced by normalize_timestamp in document.rs
    if s.len() == 24 && s.as_bytes().get(10) == Some(&b'T') && s.ends_with(".000Z") {
        format!("{} {}", &s[..10], &s[11..19])
    } else {
        s.to_string()
    }
}

/// Find documents matching a query.
pub fn find(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, query: &FindQuery, locale_ctx: Option<&LocaleContext>) -> Result<Vec<Document>> {
    validate_query_fields(def, query, locale_ctx)?;

    let (select_exprs, result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => get_locale_select_columns(&def.fields, def.timestamps, ctx),
        _ => {
            let names = get_column_names(def);
            (names.clone(), names)
        }
    };

    let (select_exprs, result_names) = apply_select_filter(
        select_exprs, result_names, query.select.as_ref(), def,
    );

    let mut sql = format!("SELECT {} FROM {}", select_exprs.join(", "), slug);
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    // Build WHERE with locale-resolved column names
    let resolved_filters = resolve_filters(&query.filters, def, locale_ctx);
    let where_clause = build_where_clause(&resolved_filters, slug, &def.fields, &mut params)?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
    }

    // FTS5 full-text search filter
    if let Some(ref search_term) = query.search {
        if let Some((fts_clause, sanitized)) = super::fts::fts_where_clause(conn, slug, search_term) {
            if where_clause.is_empty() {
                sql.push_str(&format!(" WHERE {}", fts_clause));
            } else {
                sql.push_str(&format!(" AND {}", fts_clause));
            }
            params.push(Box::new(sanitized));
        }
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
                cursor.sort_col, sort_col
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
        sql.push_str(&format!(" ORDER BY {} {}, id {}", resolved_col, effective_dir, effective_dir));
    } else {
        sql.push_str(&format!(" ORDER BY id {}", effective_dir));
    }

    if let Some(limit) = query.limit {
        sql.push_str(&format!(" LIMIT {}", limit));
    }
    if let Some(offset) = query.offset {
        sql.push_str(&format!(" OFFSET {}", offset));
    }

    let mut stmt = conn.prepare(&sql)
        .with_context(|| format!("Failed to prepare query: {}", sql))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(params_from_iter(param_refs.iter()), |row| {
        row_to_document(row, &result_names)
    }).with_context(|| format!("Failed to execute query on '{}'", slug))?;

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
        documents.push(doc);
    }

    // before_cursor: results were fetched in reversed order, restore correct sort order
    if using_before {
        documents.reverse();
    }

    Ok(documents)
}

/// Find a single document by ID with full hydration (join tables, group reconstruction).
///
/// This is the standard read function — returns a fully hydrated document with
/// nested group objects and populated join table data (arrays, blocks, relationships).
/// Use `find_by_id_raw` when you only need flat column data without hydration.
pub fn find_by_id(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, id: &str, locale_ctx: Option<&LocaleContext>) -> Result<Option<Document>> {
    let doc = find_by_id_raw(conn, slug, def, id, locale_ctx)?;
    match doc {
        Some(mut d) => {
            super::hydrate_document(conn, slug, &def.fields, &mut d, None, locale_ctx)?;
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
        super::hydrate_document(conn, slug, &def.fields, &mut doc, None, locale_ctx)?;
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

/// Count documents in a collection.
pub fn count(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, filters: &[FilterClause], locale_ctx: Option<&LocaleContext>) -> Result<i64> {
    count_with_search(conn, slug, def, filters, locale_ctx, None)
}

/// Count documents with optional FTS search filter.
pub fn count_with_search(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, filters: &[FilterClause], locale_ctx: Option<&LocaleContext>, search: Option<&str>) -> Result<i64> {
    let (exact, prefixes) = super::get_valid_filter_paths(def, locale_ctx);
    for clause in filters {
        match clause {
            FilterClause::Single(f) => super::validate_filter_field(&f.field, &exact, &prefixes)?,
            FilterClause::Or(groups) => {
                for group in groups {
                    for f in group {
                        super::validate_filter_field(&f.field, &exact, &prefixes)?;
                    }
                }
            }
        }
    }

    let mut sql = format!("SELECT COUNT(*) FROM {}", slug);
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let resolved_filters = resolve_filters(filters, def, locale_ctx);
    let where_clause = build_where_clause(&resolved_filters, slug, &def.fields, &mut params)?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
    }

    // FTS5 full-text search filter
    if let Some(search_term) = search {
        if let Some((fts_clause, sanitized)) = super::fts::fts_where_clause(conn, slug, search_term) {
            if where_clause.is_empty() {
                sql.push_str(&format!(" WHERE {}", fts_clause));
            } else {
                sql.push_str(&format!(" AND {}", fts_clause));
            }
            params.push(Box::new(sanitized));
        }
    }

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let count: i64 = conn.query_row(&sql, params_from_iter(param_refs.iter()), |row| row.get(0))
        .with_context(|| format!("Failed to count documents in '{}'", slug))?;

    Ok(count)
}

/// Count rows where a field equals a value, optionally excluding an ID.
/// Used for unique constraint validation.
pub fn count_where_field_eq(
    conn: &rusqlite::Connection,
    table: &str,
    field: &str,
    value: &str,
    exclude_id: Option<&str>,
) -> Result<i64> {
    if !is_valid_identifier(field) {
        bail!("Invalid field name '{}': must be alphanumeric/underscore", field);
    }
    let (sql, count) = match exclude_id {
        Some(eid) => {
            let sql = format!(
                "SELECT COUNT(*) FROM {} WHERE {} = ?1 AND id != ?2",
                table, field
            );
            let c: i64 = conn.query_row(&sql, rusqlite::params![value, eid], |row| row.get(0))
                .with_context(|| format!("Unique check on {}.{}", table, field))?;
            (sql, c)
        }
        None => {
            let sql = format!(
                "SELECT COUNT(*) FROM {} WHERE {} = ?1",
                table, field
            );
            let c: i64 = conn.query_row(&sql, [value], |row| row.get(0))
                .with_context(|| format!("Unique check on {}.{}", table, field))?;
            (sql, c)
        }
    };
    let _ = sql;
    Ok(count)
}

/// Filter SELECT columns based on a `select` list. If `select` is None or empty,
/// returns all columns (backward compat). Always includes `id`, `created_at`, `updated_at`.
/// For group fields: selecting `"seo"` includes all `seo__*` sub-columns.
pub fn apply_select_filter(
    select_exprs: Vec<String>,
    result_names: Vec<String>,
    select: Option<&Vec<String>>,
    def: &CollectionDefinition,
) -> (Vec<String>, Vec<String>) {
    let select = match select {
        Some(s) if !s.is_empty() => s,
        _ => return (select_exprs, result_names),
    };

    // Build set of group field names for prefix matching
    let group_names: HashSet<&str> = def.fields.iter()
        .filter(|f| f.field_type == FieldType::Group)
        .map(|f| f.name.as_str())
        .collect();

    let mut out_exprs = Vec::new();
    let mut out_names = Vec::new();

    for (expr, name) in select_exprs.into_iter().zip(result_names.into_iter()) {
        // Always include system columns
        if name == "id" || name == "created_at" || name == "updated_at" {
            out_exprs.push(expr);
            out_names.push(name);
            continue;
        }

        // Check if the result name is directly selected
        if select.iter().any(|s| s == &name) {
            out_exprs.push(expr);
            out_names.push(name);
            continue;
        }

        // Check group prefix: if select contains "seo" and name is "seo__title"
        if let Some(prefix) = name.split("__").next() {
            if group_names.contains(prefix) && select.iter().any(|s| s == prefix) {
                out_exprs.push(expr);
                out_names.push(name);
                continue;
            }
        }

        // Check locale suffix
        let base = name.split("__").next().unwrap_or(&name);
        if base != name && !group_names.contains(base) && select.iter().any(|s| s == base) {
            out_exprs.push(expr);
            out_names.push(name);
            continue;
        }
    }

    (out_exprs, out_names)
}

/// Strip fields not in `select` from a document. Always keeps `id`.
/// Used for post-query field stripping (e.g., after `find_by_id`).
pub fn apply_select_to_document(doc: &mut Document, select: &[String]) {
    doc.fields.retain(|key, _| {
        if select.iter().any(|s| s == key) {
            return true;
        }
        if let Some(prefix) = key.split("__").next() {
            if prefix != key && select.iter().any(|s| s == prefix) {
                return true;
            }
        }
        false
    });
    if !select.iter().any(|s| s == "created_at") {
        doc.created_at = None;
    }
    if !select.iter().any(|s| s == "updated_at") {
        doc.updated_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::collections::HashMap;
    use crate::core::Document;
    use crate::core::collection::*;
    use crate::core::field::*;
    use super::super::{FilterClause, Filter, FilterOp, FindQuery};
    use super::super::write::create;

    fn test_def() -> CollectionDefinition {
        CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "title".to_string(),
                    ..Default::default()
                },
                FieldDefinition {
                    name: "status".to_string(),
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
        versions: None,
            indexes: Vec::new(),
        }
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

        let query = FindQuery {
            filters: vec![FilterClause::Single(Filter {
                field: "status".to_string(),
                op: FilterOp::Equals("published".to_string()),
            })],
            ..Default::default()
        };

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
        let query_limit = FindQuery {
            limit: Some(2),
            ..Default::default()
        };
        let docs = find(&conn, "posts", &def, &query_limit, None).unwrap();
        assert_eq!(docs.len(), 2);

        // Test limit + offset (SQLite requires LIMIT before OFFSET)
        let query_offset = FindQuery {
            limit: Some(10),
            offset: Some(1),
            ..Default::default()
        };
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
        let query = FindQuery {
            order_by: Some("-title".to_string()),
            ..Default::default()
        };
        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0].get_str("title"), Some("Charlie"));
        assert_eq!(docs[1].get_str("title"), Some("Bravo"));
        assert_eq!(docs[2].get_str("title"), Some("Alpha"));
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
    fn count_empty() {
        let conn = setup_db();
        let def = test_def();

        let c = count(&conn, "posts", &def, &[], None).unwrap();
        assert_eq!(c, 0);
    }

    #[test]
    fn count_with_filter() {
        let conn = setup_db();
        let def = test_def();

        let mut d1 = HashMap::new();
        d1.insert("status".to_string(), "draft".to_string());
        create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = HashMap::new();
        d2.insert("status".to_string(), "published".to_string());
        create(&conn, "posts", &def, &d2, None).unwrap();

        let mut d3 = HashMap::new();
        d3.insert("status".to_string(), "draft".to_string());
        create(&conn, "posts", &def, &d3, None).unwrap();

        let filters = vec![FilterClause::Single(Filter {
            field: "status".to_string(),
            op: FilterOp::Equals("draft".to_string()),
        })];

        let c = count(&conn, "posts", &def, &filters, None).unwrap();
        assert_eq!(c, 2);
    }

    #[test]
    fn count_where_field_eq_basic() {
        let conn = setup_db();
        let def = test_def();
        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "AAA".to_string());
        d1.insert("status".to_string(), "draft".to_string());
        create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = HashMap::new();
        d2.insert("title".to_string(), "BBB".to_string());
        d2.insert("status".to_string(), "draft".to_string());
        let doc2 = create(&conn, "posts", &def, &d2, None).unwrap();

        let c = count_where_field_eq(&conn, "posts", "status", "draft", None).unwrap();
        assert_eq!(c, 2);

        // Exclude one
        let c_excl = count_where_field_eq(&conn, "posts", "status", "draft", Some(&doc2.id)).unwrap();
        assert_eq!(c_excl, 1);
    }

    #[test]
    fn count_where_field_eq_invalid_field_name() {
        let conn = setup_db();
        let result = count_where_field_eq(&conn, "posts", "bad field!", "val", None);
        assert!(result.is_err(), "Invalid field name should error");
        assert!(result.unwrap_err().to_string().contains("Invalid field name"));
    }

    #[test]
    fn apply_select_filter_with_group() {
        let def = CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition { name: "title".to_string(), ..Default::default() },
                FieldDefinition {
                    name: "seo".to_string(),
                    field_type: FieldType::Group,
                    fields: vec![
                        FieldDefinition { name: "meta_title".to_string(), ..Default::default() },
                        FieldDefinition { name: "meta_desc".to_string(), ..Default::default() },
                    ],
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        };

        let select_exprs = vec![
            "id".to_string(), "title".to_string(),
            "seo__meta_title".to_string(), "seo__meta_desc".to_string(),
            "created_at".to_string(), "updated_at".to_string(),
        ];
        let result_names = select_exprs.clone();

        // Select only "seo" — should include all seo__* sub-columns
        let select = vec!["seo".to_string()];
        let (exprs, names) = apply_select_filter(select_exprs, result_names, Some(&select), &def);

        assert!(names.contains(&"id".to_string()));
        assert!(names.contains(&"seo__meta_title".to_string()));
        assert!(names.contains(&"seo__meta_desc".to_string()));
        assert!(names.contains(&"created_at".to_string()));
        assert!(!names.contains(&"title".to_string()));
        assert_eq!(exprs.len(), names.len());
    }

    #[test]
    fn apply_select_filter_none_returns_all() {
        let def = test_def();
        let exprs = vec!["id".to_string(), "title".to_string(), "status".to_string()];
        let names = exprs.clone();
        let (out_exprs, out_names) = apply_select_filter(exprs.clone(), names.clone(), None, &def);
        assert_eq!(out_exprs, exprs);
        assert_eq!(out_names, names);
    }

    #[test]
    fn apply_select_filter_empty_returns_all() {
        let def = test_def();
        let exprs = vec!["id".to_string(), "title".to_string()];
        let names = exprs.clone();
        let empty: Vec<String> = Vec::new();
        let (out_exprs, out_names) = apply_select_filter(exprs.clone(), names.clone(), Some(&empty), &def);
        assert_eq!(out_exprs, exprs);
        assert_eq!(out_names, names);
    }

    #[test]
    fn apply_select_to_document_keeps_selected() {
        let mut doc = Document {
            id: "abc".to_string(),
            fields: HashMap::from([
                ("title".to_string(), serde_json::json!("Hello")),
                ("status".to_string(), serde_json::json!("draft")),
                ("body".to_string(), serde_json::json!("Some content")),
            ]),
            created_at: Some("2024-01-01".to_string()),
            updated_at: Some("2024-01-02".to_string()),
        };

        let select = vec!["title".to_string()];
        apply_select_to_document(&mut doc, &select);

        // id is always kept (not in fields HashMap, it's a struct field)
        assert_eq!(doc.id, "abc");
        // title was selected, should be kept
        assert!(doc.fields.contains_key("title"));
        // status and body were NOT selected, should be removed
        assert!(!doc.fields.contains_key("status"));
        assert!(!doc.fields.contains_key("body"));
        // timestamps not in select, should be cleared
        assert!(doc.created_at.is_none());
        assert!(doc.updated_at.is_none());
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

    // ── Cursor pagination tests ─────────────────────────────────────────────

    #[test]
    fn cursor_and_offset_mutual_exclusion() {
        let conn = setup_db();
        let def = test_def();

        let query = FindQuery {
            after_cursor: Some(super::super::cursor::CursorData {
                sort_col: "id".to_string(),
                sort_dir: "ASC".to_string(),
                sort_val: serde_json::json!("abc"),
                id: "abc".to_string(),
            }),
            offset: Some(10),
            ..Default::default()
        };
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mutually exclusive"));
    }

    #[test]
    fn cursor_asc_pagination() {
        let conn = setup_db();
        let def = test_def();

        // Insert 5 rows with deterministic titles
        let mut ids = Vec::new();
        for i in 1..=5 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            let doc = create(&conn, "posts", &def, &data, None).unwrap();
            ids.push(doc.id.clone());
        }

        // First page: limit=2, order by title ASC
        let q1 = FindQuery {
            order_by: Some("title".to_string()),
            limit: Some(2),
            ..Default::default()
        };
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].get_str("title"), Some("Post 01"));
        assert_eq!(page1[1].get_str("title"), Some("Post 02"));

        // Second page via cursor from last doc of page 1
        let last = &page1[1];
        let cursor = super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(last.get_str("title").unwrap()),
            id: last.id.clone(),
        };
        let q2 = FindQuery {
            order_by: Some("title".to_string()),
            limit: Some(2),
            after_cursor: Some(cursor),
            ..Default::default()
        };
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Third page
        let last2 = &page2[1];
        let cursor2 = super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(last2.get_str("title").unwrap()),
            id: last2.id.clone(),
        };
        let q3 = FindQuery {
            order_by: Some("title".to_string()),
            limit: Some(2),
            after_cursor: Some(cursor2),
            ..Default::default()
        };
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
        let q1 = FindQuery {
            order_by: Some("-title".to_string()),
            limit: Some(2),
            ..Default::default()
        };
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Second page via cursor
        let last = &page1[1];
        let cursor = super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "DESC".to_string(),
            sort_val: serde_json::json!(last.get_str("title").unwrap()),
            id: last.id.clone(),
        };
        let q2 = FindQuery {
            order_by: Some("-title".to_string()),
            limit: Some(2),
            after_cursor: Some(cursor),
            ..Default::default()
        };
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));
    }

    #[test]
    fn cursor_wrong_sort_col_errors() {
        let conn = setup_db();
        let def = test_def();

        let query = FindQuery {
            order_by: Some("title".to_string()),
            after_cursor: Some(super::super::cursor::CursorData {
                sort_col: "status".to_string(),
                sort_dir: "ASC".to_string(),
                sort_val: serde_json::json!("x"),
                id: "abc".to_string(),
            }),
            ..Default::default()
        };
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not match"));
    }

    #[test]
    fn before_cursor_asc_backward_pagination() {
        let conn = setup_db();
        let def = test_def();

        let mut ids = Vec::new();
        for i in 1..=5 {
            let mut data = HashMap::new();
            data.insert("title".to_string(), format!("Post {:02}", i));
            let doc = create(&conn, "posts", &def, &data, None).unwrap();
            ids.push(doc.id.clone());
        }

        // Forward: get page 2 (Posts 03, 04) so we have a cursor to go backward from
        let page1 = find(&conn, "posts", &def, &FindQuery {
            order_by: Some("title".to_string()),
            limit: Some(2),
            ..Default::default()
        }, None).unwrap();
        let last_p1 = &page1[1];
        let fwd_cursor = super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(last_p1.get_str("title").unwrap()),
            id: last_p1.id.clone(),
        };
        let page2 = find(&conn, "posts", &def, &FindQuery {
            order_by: Some("title".to_string()),
            limit: Some(2),
            after_cursor: Some(fwd_cursor),
            ..Default::default()
        }, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 03"));
        assert_eq!(page2[1].get_str("title"), Some("Post 04"));

        // Backward: from the first doc of page 2, go backward
        let first_p2 = &page2[0];
        let back_cursor = super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!(first_p2.get_str("title").unwrap()),
            id: first_p2.id.clone(),
        };
        let back_page = find(&conn, "posts", &def, &FindQuery {
            order_by: Some("title".to_string()),
            limit: Some(2),
            before_cursor: Some(back_cursor),
            ..Default::default()
        }, None).unwrap();

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
        let page1 = find(&conn, "posts", &def, &FindQuery {
            order_by: Some("-title".to_string()),
            limit: Some(2),
            ..Default::default()
        }, None).unwrap();
        assert_eq!(page1[0].get_str("title"), Some("Post 04"));
        assert_eq!(page1[1].get_str("title"), Some("Post 03"));

        // Forward DESC page 2: Posts 02, 01
        let last_p1 = &page1[1];
        let fwd_cursor = super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "DESC".to_string(),
            sort_val: serde_json::json!(last_p1.get_str("title").unwrap()),
            id: last_p1.id.clone(),
        };
        let page2 = find(&conn, "posts", &def, &FindQuery {
            order_by: Some("-title".to_string()),
            limit: Some(2),
            after_cursor: Some(fwd_cursor),
            ..Default::default()
        }, None).unwrap();
        assert_eq!(page2[0].get_str("title"), Some("Post 02"));
        assert_eq!(page2[1].get_str("title"), Some("Post 01"));

        // Backward from page 2 first doc → should get page 1 back
        let first_p2 = &page2[0];
        let back_cursor = super::super::cursor::CursorData {
            sort_col: "title".to_string(),
            sort_dir: "DESC".to_string(),
            sort_val: serde_json::json!(first_p2.get_str("title").unwrap()),
            id: first_p2.id.clone(),
        };
        let back_page = find(&conn, "posts", &def, &FindQuery {
            order_by: Some("-title".to_string()),
            limit: Some(2),
            before_cursor: Some(back_cursor),
            ..Default::default()
        }, None).unwrap();

        // Should get Posts 04, 03 in DESC order
        assert_eq!(back_page.len(), 2);
        assert_eq!(back_page[0].get_str("title"), Some("Post 04"));
        assert_eq!(back_page[1].get_str("title"), Some("Post 03"));
    }

    #[test]
    fn after_and_before_cursor_mutual_exclusion() {
        let conn = setup_db();
        let def = test_def();

        let cursor = super::super::cursor::CursorData {
            sort_col: "id".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::json!("abc"),
            id: "abc".to_string(),
        };
        let query = FindQuery {
            after_cursor: Some(cursor.clone()),
            before_cursor: Some(cursor),
            ..Default::default()
        };
        let result = find(&conn, "posts", &def, &query, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mutually exclusive"));
    }

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
}
