//! Top-level [`find`] orchestrator and the small SELECT/limit/map helpers
//! that don't fit the cursor or sort submodules.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};

use super::cursor::{SortInfo, apply_cursor_keyset};
use super::sort::{apply_order_by, resolve_sort};
use crate::core::CollectionDefinition;
use crate::core::Document;
use crate::db::query::filter::{build_where_clause, resolve_filter_column, resolve_filters};
use crate::db::query::read::select::apply_select_filter;
use crate::db::query::{
    fts, get_column_names, get_locale_select_columns_full, group_locale_fields,
    helpers::{append_sql_condition, quote_ident},
    validate_query_fields,
};
use crate::db::{
    DbConnection, DbRow, DbValue, FindQuery, LocaleContext, LocaleMode, document::row_to_document,
};

/// Find documents matching a query.
///
/// # Errors
///
/// Returns an error if any filter/sort field is invalid, or a backend error
/// if the SELECT, row parsing, or hydration fails.
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

/// Find only the **IDs** of documents matching `query`'s filters — no sort,
/// cursor, limit, or nested hydration.
///
/// Reuses the exact same filter/FTS/soft-delete logic as [`find`] (so deep
/// filters on array/block/relationship sub-fields still match via their
/// EXISTS subqueries), but selects a single `id` column and skips the
/// per-document join-table hydration that [`find`] performs. Used by bulk
/// update to collect the match-set upfront without materializing every
/// matching document in memory.
///
/// # Errors
///
/// Returns an error if any filter field is invalid, or a backend error if
/// the query fails.
pub fn find_ids(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Vec<String>> {
    validate_query_fields(def, query, locale_ctx)?;

    let mut sql = format!("SELECT id FROM \"{slug}\"");
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

    let rows = conn
        .query_all(&sql, &params)
        .with_context(|| format!("Failed to execute id query on '{slug}'"))?;

    rows.iter().map(|row| row.get_string("id")).collect()
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
            let quoted = names.iter().map(|n| quote_ident(n)).collect();
            (quoted, names)
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
        let _ = write!(sql, " LIMIT {ph}");
    }

    if let Some(offset) = query.offset {
        let ph = conn.placeholder(params.len() + 1);
        params.push(DbValue::Integer(offset.max(0)));
        let _ = write!(sql, " OFFSET {ph}");
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

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;
    use crate::config::{CrapConfig, DatabaseConfig};
    use crate::core::DocumentFields;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::query::cursor::{CursorData, SortDirection, build_cursors};
    use crate::db::query::read::find::test_helpers::*;
    use crate::db::query::write::create;
    use crate::db::{DbPool, Filter, FilterClause, FilterOp, FindQuery, pool};

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

        let mut data1 = DocumentFields::new();
        data1.insert("title".to_string(), json!("Post A"));
        data1.insert("status".to_string(), json!("draft"));
        create(&conn, "posts", &def, &data1, None).unwrap();

        let mut data2 = DocumentFields::new();
        data2.insert("title".to_string(), json!("Post B"));
        data2.insert("status".to_string(), json!("published"));
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
            let mut data = DocumentFields::new();
            data.insert("title".to_string(), Value::String(format!("Post {i}")));
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

        let mut data_a = DocumentFields::new();
        data_a.insert("title".to_string(), json!("Alpha"));
        create(&conn, "posts", &def, &data_a, None).unwrap();

        let mut data_c = DocumentFields::new();
        data_c.insert("title".to_string(), json!("Charlie"));
        create(&conn, "posts", &def, &data_c, None).unwrap();

        let mut data_b = DocumentFields::new();
        data_b.insert("title".to_string(), json!("Bravo"));
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

        let mut d1 = DocumentFields::new();
        d1.insert("title".to_string(), json!("A"));
        create(&conn, "posts", &def, &d1, None).unwrap();

        let mut d2 = DocumentFields::new();
        d2.insert("title".to_string(), json!("B"));
        create(&conn, "posts", &def, &d2, None).unwrap();

        // Sorting by "id" should use single ORDER BY clause (not the tiebreaker form)
        let q = FindQuery::builder()
            .order_by(Some("id".to_string()))
            .build();
        let docs = find(&conn, "posts", &def, &q, None).unwrap();
        assert_eq!(docs.len(), 2);
        // Just verify it executes successfully — order is nanoid-determined
    }

    // ── Soft-delete filtering ─────────────────────────────────────────

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

    /// Regression: `find_ids` must apply the same soft-delete predicate as
    /// `find`. If it didn't, bulk delete/update would silently act on trashed
    /// documents. Asserts parity against `find` in BOTH modes.
    #[test]
    fn find_ids_matches_find_for_soft_delete() {
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

        // Default: soft-deleted rows are excluded by both.
        let default_q = FindQuery::default();
        let find_ids_default = sorted_ids(&conn, &def, &default_q);
        assert_eq!(find_ids_default, vec!["id-live".to_string()]);
        assert_eq!(find_ids_default, find_id_set(&conn, &def, &default_q));

        // include_deleted: trashed rows surface in both.
        let all_q = FindQuery::builder().include_deleted(true).build();
        let find_ids_all = sorted_ids(&conn, &def, &all_q);
        assert_eq!(
            find_ids_all,
            vec!["id-deleted".to_string(), "id-live".to_string()]
        );
        assert_eq!(find_ids_all, find_id_set(&conn, &def, &all_q));
    }

    /// `find_ids` result, sorted for stable comparison.
    fn sorted_ids(
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        query: &FindQuery,
    ) -> Vec<String> {
        let mut ids = find_ids(conn, "articles", def, query, None).unwrap();
        ids.sort();
        ids
    }

    /// `find`'s result projected to a sorted id set, for parity comparison.
    fn find_id_set(
        conn: &dyn DbConnection,
        def: &CollectionDefinition,
        query: &FindQuery,
    ) -> Vec<String> {
        let mut ids: Vec<String> = find(conn, "articles", def, query, None)
            .unwrap()
            .into_iter()
            .map(|d| d.id.to_string())
            .collect();
        ids.sort();
        ids
    }

    // ── Drafts surface to the top in admin-style listings ───────────────

    fn setup_drafts_db() -> (TempDir, DbPool) {
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
                    published_at TEXT,
                    _status TEXT NOT NULL DEFAULT 'published',
                    created_at TEXT,
                    updated_at TEXT
                )",
            )
            .unwrap();
        (tmp, db_pool)
    }

    fn drafts_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.versions = Some(VersionsConfig::new(true, 0));
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("published_at", FieldType::Date).build(),
        ];
        def
    }

    /// Insert a row with an explicit `_status` (the column default is
    /// `published`, so the draft case needs an UPDATE the way
    /// `set_document_status` does).
    fn insert_post(conn: &dyn DbConnection, id: &str, status: &str, published_at: Option<&str>) {
        conn.execute(
            "INSERT INTO posts (id, title, published_at, _status) VALUES (?1, ?2, ?3, ?4)",
            &[
                DbValue::Text(id.to_string()),
                DbValue::Text(id.to_string()),
                published_at.map_or(DbValue::Null, |s| DbValue::Text(s.to_string())),
                DbValue::Text(status.to_string()),
            ],
        )
        .unwrap();
    }

    /// Regression for the "drafts hide on page 2" UX bug. With
    /// `default_sort = "-published_at"` on a drafts-enabled collection,
    /// drafts have NULL `published_at` and `SQLite` sorts NULLs LAST in
    /// DESC. With page-1 limits in the typical 20-row range, drafts
    /// vanish off the bottom of the list — making "All" look like
    /// "published only" until the user explicitly paginates. Fix:
    /// `apply_order_by` prepends `_status ASC` (which orders 'draft'
    /// before 'published') when the collection has drafts and no
    /// cursor is active. Drafts always surface above published in the
    /// page-1 view regardless of the configured `default_sort`.
    #[test]
    fn drafts_sort_above_published_in_admin_list() {
        let (_tmp, pool) = setup_drafts_db();
        let conn = pool.get().unwrap();
        let def = drafts_def();

        // 3 published rows, 2 drafts (NULL published_at).
        insert_post(&conn, "p3", "published", Some("2024-03-01T00:00:00Z"));
        insert_post(&conn, "p2", "published", Some("2024-02-01T00:00:00Z"));
        insert_post(&conn, "p1", "published", Some("2024-01-01T00:00:00Z"));
        insert_post(&conn, "d1", "draft", None);
        insert_post(&conn, "d2", "draft", None);

        let query = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .build();
        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 5);

        let statuses: Vec<&str> = docs
            .iter()
            .map(|d| d.fields.get("_status").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(
            &statuses[..2],
            &["draft", "draft"],
            "drafts must come first in admin list, got {statuses:?}"
        );
        assert_eq!(
            &statuses[2..],
            &["published", "published", "published"],
            "published rows follow drafts, got {statuses:?}"
        );
    }

    /// Regression: cursor pagination round-trip must preserve drafts on
    /// page 1. Before composite cursors, `apply_order_by`'s `_status ASC`
    /// prepend was gated on `!cursor_active`, so going forward via
    /// `after_cursor` and then back via `before_cursor` ran the keyset
    /// against `published_at` only. NULL-`published_at` drafts didn't
    /// satisfy the keyset (`NULL < cursor_pub_at` is unknown → false),
    /// so prev returned only published rows — the drafts that were on
    /// page 1 vanished.
    ///
    /// Fix: `_status ASC` is always prepended when `has_drafts`; cursors
    /// encode `_status` as `status_val`; `apply_cursor_keyset` builds
    /// a composite `(_status outer_op cursor_status) OR (_status =
    /// cursor_status AND inner_keyset)`. Drafts ride along on
    /// `before_cursor`.
    #[test]
    fn cursor_round_trip_preserves_drafts_on_page_1() {
        let (_tmp, pool) = setup_drafts_db();
        let conn = pool.get().unwrap();
        let def = drafts_def();

        // 5 published with stable published_at, 2 drafts (NULL).
        for i in 1..=5 {
            insert_post(
                &conn,
                &format!("p{i}"),
                "published",
                Some(&format!("2024-0{i}-01T00:00:00Z")),
            );
        }
        insert_post(&conn, "d1", "draft", None);
        insert_post(&conn, "d2", "draft", None);

        // Page 1: 4 rows, no cursor. Expect drafts first.
        let limit = 4;
        let q1 = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .limit(Some(limit))
            .build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        assert_eq!(page1.len(), limit as usize);
        let p1_statuses: Vec<&str> = page1
            .iter()
            .map(|d| d.fields.get("_status").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(
            &p1_statuses[..2],
            &["draft", "draft"],
            "page 1 should lead with drafts, got {p1_statuses:?}"
        );

        // Forward to page 2 via after_cursor on page 1's last row.
        let (_, end_cursor_p1) = build_cursors(&page1, "published_at", SortDirection::Desc, true);
        let end_cursor_data = CursorData::decode(&end_cursor_p1.unwrap()).unwrap();
        assert_eq!(
            end_cursor_data.status_val.as_deref(),
            Some("published"),
            "page 1's last cursor must encode the boundary _status"
        );

        let q2 = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .limit(Some(limit))
            .after_cursor(Some(end_cursor_data))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        assert_eq!(page2.len(), 3, "page 2 has 3 remaining published");
        for doc in &page2 {
            let s = doc.fields.get("_status").and_then(|v| v.as_str()).unwrap();
            assert_eq!(s, "published");
        }

        // Back to page 1 via before_cursor on page 2's first row.
        let (start_cursor_p2, _) = build_cursors(&page2, "published_at", SortDirection::Desc, true);
        let start_cursor_data = CursorData::decode(&start_cursor_p2.unwrap()).unwrap();

        let q_back = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .limit(Some(limit))
            .before_cursor(Some(start_cursor_data))
            .build();
        let page1_again = find(&conn, "posts", &def, &q_back, None).unwrap();

        assert_eq!(
            page1_again.len(),
            limit as usize,
            "back-to-page-1 should return {limit} rows, got {}",
            page1_again.len()
        );
        let again_statuses: Vec<&str> = page1_again
            .iter()
            .map(|d| d.fields.get("_status").and_then(|v| v.as_str()).unwrap())
            .collect();
        assert_eq!(
            &again_statuses[..2],
            &["draft", "draft"],
            "drafts must reappear on round-trip back to page 1, got {again_statuses:?}"
        );
        // The exact order should match the initial page 1.
        let p1_ids: Vec<&str> = page1.iter().map(|d| d.id.as_ref()).collect();
        let again_ids: Vec<&str> = page1_again.iter().map(|d| d.id.as_ref()).collect();
        assert_eq!(p1_ids, again_ids, "round-trip page 1 must equal initial");
    }

    /// Backward compatibility: a pre-composite cursor URL (no
    /// `status_val`) must still execute on a drafts-enabled collection
    /// without erroring. Behaviour degrades to single-column keyset —
    /// drafts paginated to follow won't surface — which matches what
    /// the bookmarked URL would have returned before the upgrade.
    #[test]
    fn legacy_cursor_without_status_val_still_works_on_drafts_collection() {
        let (_tmp, pool) = setup_drafts_db();
        let conn = pool.get().unwrap();
        let def = drafts_def();

        for i in 1..=3 {
            insert_post(
                &conn,
                &format!("p{i}"),
                "published",
                Some(&format!("2024-0{i}-01T00:00:00Z")),
            );
        }

        // Hand-rolled legacy cursor: status_val absent (None).
        let legacy = CursorData {
            sort_col: "published_at".to_string(),
            sort_dir: SortDirection::Desc,
            sort_val: crate::db::query::SortValue::Text("2024-03-01T00:00:00Z".to_string()),
            id: "p3".to_string(),
            status_val: None,
        };

        let q = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .limit(Some(10))
            .after_cursor(Some(legacy))
            .build();

        // Must not panic / 500 — the legacy cursor falls back to the
        // single-column keyset path. With sort_dir=Desc and op="<",
        // we expect rows whose `published_at < 2024-03-01`.
        let docs = find(&conn, "posts", &def, &q, None).unwrap();
        let ids: Vec<&str> = docs.iter().map(|d| d.id.as_ref()).collect();
        assert_eq!(
            ids,
            vec!["p2", "p1"],
            "legacy cursor must navigate published rows; got {ids:?}"
        );
    }

    /// `before_cursor` issued from a draft row (NULL `published_at`)
    /// must walk back through the draft bucket using the composite
    /// keyset. Without composite ordering, the `_status DESC` under
    /// `using_before` plus the inner NULL keyset would drop sibling
    /// drafts. Pins the symmetric round-trip across the draft bucket.
    #[test]
    fn before_cursor_on_draft_walks_draft_bucket() {
        let (_tmp, pool) = setup_drafts_db();
        let conn = pool.get().unwrap();
        let def = drafts_def();

        // 3 drafts, ids ordered so id-DESC tie-break is meaningful.
        insert_post(&conn, "d1", "draft", None);
        insert_post(&conn, "d2", "draft", None);
        insert_post(&conn, "d3", "draft", None);
        insert_post(&conn, "p1", "published", Some("2024-01-01T00:00:00Z"));

        // Page 1: limit=2 — should return d3, d2 (drafts first; id DESC).
        let q1 = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .limit(Some(2))
            .build();
        let page1 = find(&conn, "posts", &def, &q1, None).unwrap();
        let p1_ids: Vec<&str> = page1.iter().map(|d| d.id.as_ref()).collect();
        assert_eq!(p1_ids, vec!["d3", "d2"]);

        // Forward: after_cursor on d2. Should return d1 then p1.
        let (_, end_p1) = build_cursors(&page1, "published_at", SortDirection::Desc, true);
        let after = CursorData::decode(&end_p1.unwrap()).unwrap();
        assert_eq!(after.status_val.as_deref(), Some("draft"));
        let q2 = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .limit(Some(2))
            .after_cursor(Some(after))
            .build();
        let page2 = find(&conn, "posts", &def, &q2, None).unwrap();
        let p2_ids: Vec<&str> = page2.iter().map(|d| d.id.as_ref()).collect();
        assert_eq!(
            p2_ids,
            vec!["d1", "p1"],
            "after_cursor on a draft must continue drafts then cross into published"
        );

        // Back: before_cursor on d1 (page 2's first row, also a draft).
        let (start_p2, _) = build_cursors(&page2, "published_at", SortDirection::Desc, true);
        let before = CursorData::decode(&start_p2.unwrap()).unwrap();
        assert_eq!(before.status_val.as_deref(), Some("draft"));
        let q_back = FindQuery::builder()
            .order_by(Some("-published_at".to_string()))
            .limit(Some(2))
            .before_cursor(Some(before))
            .build();
        let page_back = find(&conn, "posts", &def, &q_back, None).unwrap();
        let back_ids: Vec<&str> = page_back.iter().map(|d| d.id.as_ref()).collect();
        assert_eq!(
            back_ids, p1_ids,
            "before_cursor on a draft must return the original draft-bucket page in order"
        );
    }

    /// When `_status` is already pinned by a WHERE filter (the user
    /// chose Drafts only or Published only in the filter drawer), the
    /// prepended `_status ASC` is a no-op and the user's sort wins.
    /// Verify by sorting drafts-only by id ASC: insertion order
    /// `d2`, `d1` would otherwise be reordered if `_status` did
    /// anything.
    #[test]
    fn drafts_first_does_not_disturb_status_filtered_query() {
        let (_tmp, pool) = setup_drafts_db();
        let conn = pool.get().unwrap();
        let def = drafts_def();

        insert_post(&conn, "d-zzz", "draft", None);
        insert_post(&conn, "d-aaa", "draft", None);

        let query = FindQuery::builder()
            .filters(vec![FilterClause::Single(Filter {
                field: "_status".to_string(),
                op: FilterOp::Equals("draft".to_string()),
            })])
            .order_by(Some("id".to_string()))
            .build();
        let docs = find(&conn, "posts", &def, &query, None).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].id, "d-aaa");
        assert_eq!(docs[1].id, "d-zzz");
    }

    /// Regression: negative limit/offset must be clamped to 0 instead of
    /// passing undefined values to `SQLite`.
    #[test]
    fn negative_limit_and_offset_clamped_to_zero() {
        let (_tmp, pool) = setup_db();
        let conn = pool.get().unwrap();
        let def = test_def();

        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("A"));
        create(&conn, "posts", &def, &data, None).unwrap();

        let mut data2 = DocumentFields::new();
        data2.insert("title".to_string(), json!("B"));
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
