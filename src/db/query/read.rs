//! Read operations: find, find_by_id, count, select filtering.

use anyhow::{Context, Result, bail};
use rusqlite::params_from_iter;
use std::collections::HashSet;

use crate::core::{CollectionDefinition, Document};
use crate::core::field::FieldType;
use crate::db::document::row_to_document;
use super::{
    LocaleMode, LocaleContext, FindQuery, FilterClause,
    get_column_names, get_locale_select_columns, get_valid_filter_columns,
    validate_field_name, validate_query_fields, group_locale_fields,
    is_valid_identifier,
};
use super::filter::{build_where_clause, resolve_filters, resolve_filter_column};

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
    let where_clause = build_where_clause(&resolved_filters, &mut params)?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
    }

    if let Some(ref order) = query.order_by {
        let (col, dir) = if let Some(stripped) = order.strip_prefix('-') {
            (stripped, "DESC")
        } else {
            (order.as_str(), "ASC")
        };
        let resolved_col = resolve_filter_column(col, def, locale_ctx);
        sql.push_str(&format!(" ORDER BY {} {}", resolved_col, dir));
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

    Ok(documents)
}

/// Find a single document by ID.
pub fn find_by_id(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, id: &str, locale_ctx: Option<&LocaleContext>) -> Result<Option<Document>> {
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
    let valid = get_valid_filter_columns(def, locale_ctx);
    for clause in filters {
        match clause {
            FilterClause::Single(f) => validate_field_name(&f.field, &valid)?,
            FilterClause::Or(groups) => {
                for group in groups {
                    for f in group {
                        validate_field_name(&f.field, &valid)?;
                    }
                }
            }
        }
    }

    let mut sql = format!("SELECT COUNT(*) FROM {}", slug);
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let resolved_filters = resolve_filters(filters, def, locale_ctx);
    let where_clause = build_where_clause(&resolved_filters, &mut params)?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
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
                    field_type: FieldType::Text,
                    required: false,
                    unique: false,
                    validate: None,
                    default_value: None,
                    options: vec![],
                    admin: FieldAdmin::default(),
                    hooks: FieldHooks::default(),
                    access: FieldAccess::default(),
                    relationship: None,
                    fields: vec![],
                    blocks: vec![],
                    localized: false,
                },
                FieldDefinition {
                    name: "status".to_string(),
                    field_type: FieldType::Text,
                    required: false,
                    unique: false,
                    validate: None,
                    default_value: None,
                    options: vec![],
                    admin: FieldAdmin::default(),
                    hooks: FieldHooks::default(),
                    access: FieldAccess::default(),
                    relationship: None,
                    fields: vec![],
                    blocks: vec![],
                    localized: false,
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
        versions: None,
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
}
