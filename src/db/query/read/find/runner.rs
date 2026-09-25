//! Top-level [`find`] orchestrator and the small SELECT/limit/map helpers
//! that don't fit the cursor or sort submodules.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};

use super::cursor::{SortInfo, apply_cursor_keyset, check_cursor_sort_value};
use super::sort::{apply_order_by, resolve_sort};
use crate::core::CollectionDefinition;
use crate::core::Document;
use crate::db::query::filter::{build_where_clause, lookup_column_field_type};
use crate::db::query::read::{decode_row, select::apply_select_filter};
use crate::db::query::{
    column_read_expr,
    fts::{self, FtsSearch},
    get_column_names, get_locale_select_columns_full,
    helpers::{append_soft_delete_filter, append_sql_condition, quote_ident},
    validate_query_fields,
};
use crate::db::{DbConnection, DbRow, DbValue, FindQuery, LocaleContext};

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

    let where_clause = build_where_clause(
        conn,
        &query.filters,
        slug,
        &def.fields,
        locale_ctx,
        &mut params,
    )?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
        has_where = true;
    }

    let search = fts_search(slug, def, query, locale_ctx);

    if let Some(clause) = apply_fts(conn, search.as_ref(), &mut params)? {
        append_sql_condition(&mut sql, &mut has_where, &clause);
    }

    apply_soft_delete(def, query, &mut sql, &mut has_where);

    let (sort_col, sort_dir, using_before) = resolve_sort(def, query)?;

    if let Some(cursor) = query.after_cursor.as_ref().or(query.before_cursor.as_ref()) {
        let sort = SortInfo {
            col: &sort_col,
            dir: sort_dir,
            using_before,
        };

        let resolved = column_read_expr(&sort_col, &def.fields, locale_ctx)?;
        let sort_type = lookup_column_field_type(&sort_col, &def.fields);
        check_cursor_sort_value(cursor, &sort_col, sort_type.as_ref())?;

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
    if sort_col == "_rank" {
        apply_rank_order_by(conn, search.as_ref(), &mut sql, &mut params)?;
    } else {
        apply_order_by(&sort_col, sort_dir, using_before, def, locale_ctx, &mut sql)?;
    }
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

    let where_clause = build_where_clause(
        conn,
        &query.filters,
        slug,
        &def.fields,
        locale_ctx,
        &mut params,
    )?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
        has_where = true;
    }

    let search = fts_search(slug, def, query, locale_ctx);

    if let Some(clause) = apply_fts(conn, search.as_ref(), &mut params)? {
        append_sql_condition(&mut sql, &mut has_where, &clause);
    }

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
    Ok(build_select_named(def, query, locale_ctx)?.0)
}

/// Build the SELECT column list, paired with the name each expression is
/// returned under.
pub(super) fn build_select_named(
    def: &CollectionDefinition,
    query: &FindQuery,
    locale_ctx: Option<&LocaleContext>,
) -> Result<(Vec<String>, Vec<String>)> {
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

    Ok(apply_select_filter(
        select_exprs,
        result_names,
        query.select.as_ref(),
    ))
}

/// The full-text search `query` asks for on `def`, matched in the requested
/// locale's text.
fn fts_search<'a>(
    slug: &'a str,
    def: &'a CollectionDefinition,
    query: &'a FindQuery,
    locale_ctx: Option<&'a LocaleContext>,
) -> Option<FtsSearch<'a>> {
    let term = query.search.as_deref()?;

    Some(
        FtsSearch::builder(slug, def, term)
            .locale_ctx(locale_ctx)
            .build(),
    )
}

/// Sort by search relevance (`order_by = "_rank"`, best first). Falls back
/// to plain `id` order when the FTS clause is unavailable (no index yet /
/// term has no searchable word) — matching the search *filter*'s graceful
/// degradation. Note the drafts `_status ASC` prepend is intentionally
/// skipped in rank mode: the caller asked for relevance, relevance wins.
fn apply_rank_order_by(
    conn: &dyn DbConnection,
    search: Option<&FtsSearch<'_>>,
    sql: &mut String,
    params: &mut Vec<DbValue>,
) -> Result<()> {
    let clause = match search {
        Some(search) => fts::fts_rank_order_by(conn, search, params.len() + 1)?,
        None => None,
    };

    let Some((order_clause, query)) = clause else {
        sql.push_str(" ORDER BY id ASC");
        return Ok(());
    };

    sql.push_str(&order_clause);
    params.push(DbValue::Text(query));

    Ok(())
}

/// The FTS search filter condition, if `search` has one, with its query bound
/// into `params`.
fn apply_fts(
    conn: &dyn DbConnection,
    search: Option<&FtsSearch<'_>>,
    params: &mut Vec<DbValue>,
) -> Result<Option<String>> {
    let Some(search) = search else {
        return Ok(None);
    };

    let Some((clause, query)) = fts::fts_where_clause(conn, search, params.len() + 1)? else {
        return Ok(None);
    };

    params.push(DbValue::Text(query));

    Ok(Some(clause))
}

/// Exclude soft-deleted documents unless explicitly requested. Adapts the
/// `FindQuery` to the shared [`append_soft_delete_filter`] decision.
pub(super) fn apply_soft_delete(
    def: &CollectionDefinition,
    query: &FindQuery,
    sql: &mut String,
    has_where: &mut bool,
) {
    append_soft_delete_filter(def, query.include_deleted, sql, has_where);
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
pub(super) fn map_rows(
    conn: &dyn DbConnection,
    rows: &[DbRow],
    locale_ctx: Option<&LocaleContext>,
    def: &CollectionDefinition,
    using_before: bool,
) -> Result<Vec<Document>> {
    let mut documents = Vec::new();

    for row in rows {
        documents.push(decode_row(conn, row, &def.fields, locale_ctx)?);
    }

    if using_before {
        documents.reverse();
    }

    Ok(documents)
}

#[cfg(test)]
mod tests;
