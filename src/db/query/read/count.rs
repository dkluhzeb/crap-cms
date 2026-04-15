//! `count`, `count_with_search`, `count_where_field_eq` — document counting.

use anyhow::{Context as _, Result, bail};

use crate::core::CollectionDefinition;
use crate::db::{
    DbConnection, DbValue, FilterClause, LocaleContext,
    query::{
        filter::{build_where_clause, resolve_filters},
        fts, is_valid_identifier,
        validation::{get_valid_filter_paths, validate_filter_field},
    },
};

use crate::db::query::helpers::append_sql_condition;

/// Count documents in a collection.
pub fn count(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    filters: &[FilterClause],
    locale_ctx: Option<&LocaleContext>,
) -> Result<i64> {
    count_with_search(conn, slug, def, filters, locale_ctx, None, false)
}

/// Count documents with optional FTS search filter.
///
/// When `include_deleted` is true, soft-deleted documents are included in the count
/// (used by the trash view).
pub fn count_with_search(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    filters: &[FilterClause],
    locale_ctx: Option<&LocaleContext>,
    search: Option<&str>,
    include_deleted: bool,
) -> Result<i64> {
    let (exact, prefixes) = get_valid_filter_paths(def, locale_ctx);
    for clause in filters {
        match clause {
            FilterClause::Single(f) => validate_filter_field(&f.field, &exact, &prefixes)?,
            FilterClause::Or(groups) => {
                for group in groups {
                    for f in group {
                        validate_filter_field(&f.field, &exact, &prefixes)?;
                    }
                }
            }
        }
    }

    let mut sql = format!("SELECT COUNT(*) FROM \"{slug}\"");
    let mut params: Vec<DbValue> = Vec::new();

    let resolved_filters = resolve_filters(filters, def, locale_ctx)?;
    let where_clause = build_where_clause(
        conn,
        &resolved_filters,
        slug,
        &def.fields,
        locale_ctx,
        &mut params,
    )?;
    let mut has_where = !where_clause.is_empty();

    if has_where {
        sql.push_str(&where_clause);
    }

    // FTS5 full-text search filter
    if let Some((clause, sanitized)) =
        search.and_then(|term| fts::fts_where_clause(conn, slug, term, params.len() + 1))
    {
        append_sql_condition(&mut sql, &mut has_where, &clause);
        params.push(DbValue::Text(sanitized));
    }

    // Exclude soft-deleted documents unless include_deleted is set
    if def.soft_delete && !include_deleted {
        append_sql_condition(&mut sql, &mut has_where, "_deleted_at IS NULL");
    }

    let row = conn
        .query_one(&sql, &params)
        .with_context(|| format!("Failed to count documents in '{slug}'"))?;

    let count = row
        .as_ref()
        .and_then(|r| r.get_value(0))
        .and_then(|v| {
            if let DbValue::Integer(i) = v {
                Some(*i)
            } else {
                None
            }
        })
        .unwrap_or(0);

    Ok(count)
}

/// Count rows where a field equals a value, optionally excluding an ID.
/// Used for unique constraint validation.
pub fn count_where_field_eq(
    conn: &dyn DbConnection,
    table: &str,
    field: &str,
    value: &str,
    exclude_id: Option<&str>,
    soft_delete: bool,
) -> Result<i64> {
    if !is_valid_identifier(field) {
        bail!(
            "Invalid field name '{}': must be alphanumeric/underscore",
            field
        );
    }

    let soft_filter = if soft_delete {
        " AND _deleted_at IS NULL"
    } else {
        ""
    };

    let row = match exclude_id {
        Some(eid) => {
            let (p1, p2) = (conn.placeholder(1), conn.placeholder(2));
            let sql = format!(
                "SELECT COUNT(*) FROM \"{table}\" WHERE \"{field}\" = {p1} AND id != {p2}{soft_filter}"
            );
            conn.query_one(
                &sql,
                &[
                    DbValue::Text(value.to_string()),
                    DbValue::Text(eid.to_string()),
                ],
            )
            .with_context(|| format!("Unique check on {table}.{field}"))?
        }
        None => {
            let p1 = conn.placeholder(1);
            let sql =
                format!("SELECT COUNT(*) FROM \"{table}\" WHERE \"{field}\" = {p1}{soft_filter}");
            conn.query_one(&sql, &[DbValue::Text(value.to_string())])
                .with_context(|| format!("Unique check on {table}.{field}"))?
        }
    };

    let count = row
        .as_ref()
        .and_then(|r| r.get_value(0))
        .and_then(|v| {
            if let DbValue::Integer(i) = v {
                Some(*i)
            } else {
                None
            }
        })
        .unwrap_or(0);

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig};
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{DbPool, pool};
    use crate::db::{Filter, FilterClause, FilterOp, query::write::create};
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
    fn count_empty() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let c = count(&conn, "posts", &def, &[], None).unwrap();
        assert_eq!(c, 0);
    }

    #[test]
    fn count_with_filter() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
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
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "AAA".to_string());
        d1.insert("status".to_string(), "draft".to_string());
        create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = HashMap::new();
        d2.insert("title".to_string(), "BBB".to_string());
        d2.insert("status".to_string(), "draft".to_string());
        let doc2 = create(&conn, "posts", &def, &d2, None).unwrap();

        let c = count_where_field_eq(&conn, "posts", "status", "draft", None, false).unwrap();
        assert_eq!(c, 2);

        // Exclude one
        let c_excl =
            count_where_field_eq(&conn, "posts", "status", "draft", Some(&doc2.id), false).unwrap();
        assert_eq!(c_excl, 1);
    }

    #[test]
    fn count_where_field_eq_invalid_field_name() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let result = count_where_field_eq(&conn, "posts", "bad field!", "val", None, false);
        assert!(result.is_err(), "Invalid field name should error");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid field name")
        );
    }

    // ── count_with_search: FTS search path without WHERE clause ──────────

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
    fn count_excludes_soft_deleted() {
        let (_tmp, pool) = setup_soft_delete_db();
        let conn = pool.get().unwrap();
        let def = soft_delete_def();

        // Insert one live and one soft-deleted doc
        conn.execute(
            "INSERT INTO articles (id, title, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            &[
                DbValue::Text("id-live".into()),
                DbValue::Text("Live".into()),
                DbValue::Text("draft".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO articles (id, title, status, _deleted_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            &[
                DbValue::Text("id-deleted".into()),
                DbValue::Text("Deleted".into()),
                DbValue::Text("draft".into()),
                DbValue::Text("2026-01-02 00:00:00".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        let c = count(&conn, "articles", &def, &[], None).unwrap();
        assert_eq!(c, 1, "count should exclude soft-deleted documents");
    }

    #[test]
    fn count_with_filter_excludes_soft_deleted() {
        let (_tmp, pool) = setup_soft_delete_db();
        let conn = pool.get().unwrap();
        let def = soft_delete_def();

        // Two draft docs, one soft-deleted
        conn.execute(
            "INSERT INTO articles (id, title, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            &[
                DbValue::Text("id-live".into()),
                DbValue::Text("Live".into()),
                DbValue::Text("draft".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO articles (id, title, status, _deleted_at, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            &[
                DbValue::Text("id-deleted".into()),
                DbValue::Text("Deleted".into()),
                DbValue::Text("draft".into()),
                DbValue::Text("2026-01-02 00:00:00".into()),
                DbValue::Text("2026-01-01 00:00:00".into()),
            ],
        )
        .unwrap();

        let filters = vec![FilterClause::Single(Filter {
            field: "status".to_string(),
            op: FilterOp::Equals("draft".to_string()),
        })];

        let c = count(&conn, "articles", &def, &filters, None).unwrap();
        assert_eq!(
            c, 1,
            "count with filter should exclude soft-deleted documents"
        );
    }

    #[test]
    fn count_with_search_no_other_filters() {
        use crate::config::LocaleConfig;
        use crate::db::query::fts;

        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        // Create some posts
        let mut d1 = HashMap::new();
        d1.insert("title".to_string(), "Rust Tutorial".to_string());
        create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = HashMap::new();
        d2.insert("title".to_string(), "Python Tutorial".to_string());
        create(&conn, "posts", &def, &d2, None).unwrap();

        // Set up FTS
        fts::sync_fts_table(&conn, "posts", &def, &LocaleConfig::default()).unwrap();

        // count_with_search with no other filters (exercises the WHERE-less FTS code path)
        let c = count_with_search(&conn, "posts", &def, &[], None, Some("Rust"), false).unwrap();
        assert_eq!(c, 1, "FTS search should find only the Rust post");
    }
}
