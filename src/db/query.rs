//! CRUD query functions operating on `&rusqlite::Connection` (works with both plain
//! connections and transactions via `Deref`).

use anyhow::{Context, Result, bail};
use rusqlite::params_from_iter;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::config::LocaleConfig;
use crate::core::{CollectionDefinition, Document};
use crate::core::collection::GlobalDefinition;
use crate::core::field::{FieldDefinition, FieldType};
use super::document::row_to_document;

/// How to handle localized fields in a query.
#[derive(Debug, Clone)]
pub enum LocaleMode {
    /// Return only the default locale (or no locales if disabled). Flat field names.
    Default,
    /// Return a specific locale. Flat field names.
    Single(String),
    /// Return all locales. Nested objects: { en: "val", de: "val" }.
    All,
}

/// Locale context for query functions: combines config + mode.
#[derive(Debug, Clone)]
pub struct LocaleContext {
    pub mode: LocaleMode,
    pub config: LocaleConfig,
}

impl LocaleContext {
    /// Build a `LocaleContext` from an optional locale string and config.
    /// Returns `None` if localization is disabled (empty `locales` vec).
    /// `"all"` → `All`, a specific code → `Single`, `None` → `Default`.
    pub fn from_locale_string(locale: Option<&str>, config: &LocaleConfig) -> Option<Self> {
        if !config.is_enabled() {
            return None;
        }
        let mode = match locale {
            Some("all") => LocaleMode::All,
            Some(l) => LocaleMode::Single(l.to_string()),
            None => LocaleMode::Default,
        };
        Some(Self { mode, config: config.clone() })
    }
}

/// Result of an access control check.
#[derive(Debug, Clone)]
pub enum AccessResult {
    /// Access allowed, no restrictions.
    Allowed,
    /// Access denied.
    Denied,
    /// Access allowed with constraints (read only). Additional query filters to merge.
    Constrained(Vec<FilterClause>),
}

/// A filter comparison operator with its operand value(s).
#[derive(Debug, Clone)]
pub enum FilterOp {
    Equals(String),
    NotEquals(String),
    Like(String),
    Contains(String),
    GreaterThan(String),
    LessThan(String),
    GreaterThanOrEqual(String),
    LessThanOrEqual(String),
    In(Vec<String>),
    NotIn(Vec<String>),
    Exists,
    NotExists,
}

/// A single field + operator filter condition.
#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
}

/// A filter clause: either a single condition or an OR group.
/// Each OR element is a group of AND-ed filters: `(a AND b) OR (c AND d)`.
#[derive(Debug, Clone)]
pub enum FilterClause {
    Single(Filter),
    Or(Vec<Vec<Filter>>),
}

/// Parameters for a find query: filters, ordering, pagination, and field selection.
#[derive(Debug, Default, Clone)]
pub struct FindQuery {
    pub filters: Vec<FilterClause>,
    pub order_by: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Optional list of fields to return. `None` = all fields.
    /// Always includes `id`, `created_at`, `updated_at`.
    pub select: Option<Vec<String>>,
}

/// Check that a string is a safe SQL identifier (alphanumeric + underscore).
pub fn is_valid_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate that a field name exists in the set of valid columns.
pub fn validate_field_name(field: &str, valid_columns: &HashSet<String>) -> Result<()> {
    if !valid_columns.contains(field) {
        bail!(
            "Invalid field '{}'. Valid fields: {}",
            field,
            valid_columns.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

/// Validate all filter fields and order_by in a FindQuery against a collection definition.
pub fn validate_query_fields(def: &CollectionDefinition, query: &FindQuery, locale_ctx: Option<&LocaleContext>) -> Result<()> {
    let valid = get_valid_filter_columns(def, locale_ctx);

    for clause in &query.filters {
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

    if let Some(ref order) = query.order_by {
        let col = order.strip_prefix('-').unwrap_or(order);
        validate_field_name(col, &valid)?;
    }

    Ok(())
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

/// Create a new document. Returns the created document.
pub fn create(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let id = nanoid::nanoid!();
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut columns = vec!["id".to_string()];
    let mut placeholders = vec!["?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(id.clone())];
    let mut idx = 2;

    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let data_key = format!("{}__{}", field.name, sub.name);
                let col_name = locale_write_column(&data_key, sub, &locale_ctx);
                if let Some(value) = data.get(&data_key) {
                    columns.push(col_name);
                    placeholders.push(format!("?{}", idx));
                    params.push(coerce_value(&sub.field_type, value));
                    idx += 1;
                } else if sub.field_type == FieldType::Checkbox {
                    columns.push(col_name);
                    placeholders.push(format!("?{}", idx));
                    params.push(Box::new(0i32));
                    idx += 1;
                }
            }
            continue;
        }
        if !field.has_parent_column() {
            continue;
        }
        let col_name = locale_write_column(&field.name, field, &locale_ctx);
        if let Some(value) = data.get(&field.name) {
            columns.push(col_name);
            placeholders.push(format!("?{}", idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            columns.push(col_name);
            placeholders.push(format!("?{}", idx));
            params.push(Box::new(0i32));
            idx += 1;
        }
    }

    if def.timestamps {
        columns.push("created_at".to_string());
        placeholders.push(format!("?{}", idx));
        params.push(Box::new(now.clone()));
        idx += 1;

        columns.push("updated_at".to_string());
        placeholders.push(format!("?{}", idx));
        params.push(Box::new(now));
    }

    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        slug,
        columns.join(", "),
        placeholders.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to insert into '{}'", slug))?;

    // Return the created document with the same locale context
    find_by_id(conn, slug, def, &id, locale_ctx)?
        .ok_or_else(|| anyhow::anyhow!("Failed to find newly created document"))
}

/// Update a document by ID. Returns the updated document.
pub fn update(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let data_key = format!("{}__{}", field.name, sub.name);
                let col_name = locale_write_column(&data_key, sub, &locale_ctx);
                if let Some(value) = data.get(&data_key) {
                    set_clauses.push(format!("{} = ?{}", col_name, idx));
                    params.push(coerce_value(&sub.field_type, value));
                    idx += 1;
                } else if sub.field_type == FieldType::Checkbox {
                    set_clauses.push(format!("{} = ?{}", col_name, idx));
                    params.push(Box::new(0i32));
                    idx += 1;
                }
            }
            continue;
        }
        if !field.has_parent_column() {
            continue;
        }
        let col_name = locale_write_column(&field.name, field, &locale_ctx);
        if let Some(value) = data.get(&field.name) {
            set_clauses.push(format!("{} = ?{}", col_name, idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            set_clauses.push(format!("{} = ?{}", col_name, idx));
            params.push(Box::new(0i32));
            idx += 1;
        }
    }

    if def.timestamps {
        set_clauses.push(format!("updated_at = ?{}", idx));
        params.push(Box::new(now));
        idx += 1;
    }

    if set_clauses.is_empty() {
        return find_by_id(conn, slug, def, id, locale_ctx)?
            .ok_or_else(|| anyhow::anyhow!("Document not found"));
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = ?{}",
        slug,
        set_clauses.join(", "),
        idx
    );
    params.push(Box::new(id.to_string()));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to update document {} in '{}'", id, slug))?;

    find_by_id(conn, slug, def, id, locale_ctx)?
        .ok_or_else(|| anyhow::anyhow!("Document not found after update"))
}

/// Delete a document by ID.
pub fn delete(conn: &rusqlite::Connection, slug: &str, id: &str) -> Result<()> {
    let sql = format!("DELETE FROM {} WHERE id = ?1", slug);
    conn.execute(&sql, [id])
        .with_context(|| format!("Failed to delete document {} from '{}'", id, slug))?;
    Ok(())
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
    let _ = sql; // used in error context above
    Ok(count)
}

// ── Globals ──────────────────────────────────────────────────────────────────

/// Get the single global document from `_global_{slug}`.
pub fn get_global(conn: &rusqlite::Connection, slug: &str, def: &GlobalDefinition, locale_ctx: Option<&LocaleContext>) -> Result<Document> {
    let table_name = format!("_global_{}", slug);

    let (select_exprs, result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => get_locale_select_columns(&def.fields, true, ctx),
        _ => {
            let names = get_global_column_names(def);
            (names.clone(), names)
        }
    };

    let sql = format!("SELECT {} FROM {} WHERE id = 'default'", select_exprs.join(", "), table_name);

    let mut doc = conn.query_row(&sql, [], |row| {
        row_to_document(row, &result_names)
    }).with_context(|| format!("Failed to get global '{}'", slug))?;

    if let Some(ctx) = locale_ctx {
        if ctx.config.is_enabled() {
            if let LocaleMode::All = ctx.mode {
                group_locale_fields(&mut doc, &def.fields, &ctx.config);
            }
        }
    }

    Ok(doc)
}

/// Update the single global document in `_global_{slug}`. Returns the updated document.
pub fn update_global(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &GlobalDefinition,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let table_name = format!("_global_{}", slug);
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    for field in &def.fields {
        let col_name = locale_write_column(&field.name, field, &locale_ctx);
        if let Some(value) = data.get(&field.name) {
            set_clauses.push(format!("{} = ?{}", col_name, idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            set_clauses.push(format!("{} = ?{}", col_name, idx));
            params.push(Box::new(0i32));
            idx += 1;
        }
    }

    set_clauses.push(format!("updated_at = ?{}", idx));
    params.push(Box::new(now));

    if set_clauses.is_empty() {
        return get_global(conn, slug, def, locale_ctx);
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = 'default'",
        table_name,
        set_clauses.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to update global '{}'", slug))?;

    get_global(conn, slug, def, locale_ctx)
}

fn get_global_column_names(def: &GlobalDefinition) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    for field in &def.fields {
        names.push(field.name.clone());
    }
    names.push("created_at".to_string());
    names.push("updated_at".to_string());
    names
}

/// Filter SELECT columns based on a `select` list. If `select` is None or empty,
/// returns all columns (backward compat). Always includes `id`, `created_at`, `updated_at`.
/// For group fields: selecting `"seo"` includes all `seo__*` sub-columns.
fn apply_select_filter(
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

        // Check locale suffix: name might be "title__en" for a localized field "title"
        // The result_names for locale columns in All mode are "field__locale",
        // but for Single/Default mode they're aliased to "field" already.
        // We need to match the base field name against the select list.
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
        // Group field: if select contains "seo", keep "seo" (the hydrated group object)
        // Also keep sub-columns like "seo__title" if "seo" is selected (pre-hydration)
        if let Some(prefix) = key.split("__").next() {
            if prefix != key && select.iter().any(|s| s == prefix) {
                return true;
            }
        }
        false
    });
    // Strip timestamps if not selected (find_by_id returns them in the Document struct)
    if !select.iter().any(|s| s == "created_at") {
        doc.created_at = None;
    }
    if !select.iter().any(|s| s == "updated_at") {
        doc.updated_at = None;
    }
}

/// Build a single filter into a SQL condition + params.
/// Defense-in-depth: rejects field names that aren't valid identifiers, even if
/// higher-level validation should have caught them already.
fn build_filter_condition(f: &Filter, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> Result<String> {
    if !is_valid_identifier(&f.field) {
        bail!("Invalid field name '{}': must be alphanumeric/underscore", f.field);
    }
    Ok(match &f.op {
        FilterOp::Equals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} = ?", f.field)
        }
        FilterOp::NotEquals(v) => {
            params.push(Box::new(v.clone()));
            format!("{} != ?", f.field)
        }
        FilterOp::Like(v) => {
            params.push(Box::new(v.clone()));
            format!("{} LIKE ?", f.field)
        }
        FilterOp::Contains(v) => {
            let escaped = v.replace('%', "\\%").replace('_', "\\_");
            params.push(Box::new(format!("%{}%", escaped)));
            format!("{} LIKE ? ESCAPE '\\'", f.field)
        }
        FilterOp::GreaterThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} > ?", f.field)
        }
        FilterOp::LessThan(v) => {
            params.push(Box::new(v.clone()));
            format!("{} < ?", f.field)
        }
        FilterOp::GreaterThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} >= ?", f.field)
        }
        FilterOp::LessThanOrEqual(v) => {
            params.push(Box::new(v.clone()));
            format!("{} <= ?", f.field)
        }
        FilterOp::In(vals) => {
            let placeholders: Vec<_> = vals.iter().map(|v| {
                params.push(Box::new(v.clone()));
                "?".to_string()
            }).collect();
            format!("{} IN ({})", f.field, placeholders.join(", "))
        }
        FilterOp::NotIn(vals) => {
            let placeholders: Vec<_> = vals.iter().map(|v| {
                params.push(Box::new(v.clone()));
                "?".to_string()
            }).collect();
            format!("{} NOT IN ({})", f.field, placeholders.join(", "))
        }
        FilterOp::Exists => {
            format!("{} IS NOT NULL", f.field)
        }
        FilterOp::NotExists => {
            format!("{} IS NULL", f.field)
        }
    })
}

/// Build a WHERE clause from filter clauses. Returns empty string if no filters.
fn build_where_clause(filters: &[FilterClause], params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> Result<String> {
    if filters.is_empty() {
        return Ok(String::new());
    }

    let mut conditions = Vec::new();
    for clause in filters {
        match clause {
            FilterClause::Single(f) => {
                conditions.push(build_filter_condition(f, params)?);
            }
            FilterClause::Or(groups) => {
                if groups.len() == 1 && groups[0].len() == 1 {
                    conditions.push(build_filter_condition(&groups[0][0], params)?);
                } else {
                    let mut or_parts = Vec::new();
                    for group in groups {
                        if group.len() == 1 {
                            or_parts.push(build_filter_condition(&group[0], params)?);
                        } else {
                            let and_parts: Vec<String> = group.iter()
                                .map(|f| build_filter_condition(f, params))
                                .collect::<Result<_, _>>()?;
                            or_parts.push(format!("({})", and_parts.join(" AND ")));
                        }
                    }
                    conditions.push(format!("({})", or_parts.join(" OR ")));
                }
            }
        }
    }

    Ok(format!(" WHERE {}", conditions.join(" AND ")))
}

/// Coerce a form string value to the appropriate SQLite type.
fn coerce_value(field_type: &FieldType, value: &str) -> Box<dyn rusqlite::types::ToSql> {
    match field_type {
        FieldType::Checkbox => {
            let b = matches!(value, "on" | "true" | "1" | "yes");
            Box::new(b as i32)
        }
        FieldType::Number => {
            if value.is_empty() {
                Box::new(rusqlite::types::Null)
            } else if let Ok(f) = value.parse::<f64>() {
                Box::new(f)
            } else {
                Box::new(rusqlite::types::Null)
            }
        }
        _ => {
            if value.is_empty() {
                Box::new(rusqlite::types::Null)
            } else {
                Box::new(value.to_string())
            }
        }
    }
}

// ── Auth functions ────────────────────────────────────────────────────────────

/// Find a document by email in an auth collection.
pub fn find_by_email(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    email: &str,
) -> Result<Option<Document>> {
    let column_names = get_column_names(def);

    let sql = format!(
        "SELECT {} FROM {} WHERE email = ?1",
        column_names.join(", "),
        slug
    );

    let result = conn.query_row(&sql, [email], |row| {
        row_to_document(row, &column_names)
    });

    match result {
        Ok(doc) => Ok(Some(doc)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to find user by email in {}", slug)),
    }
}

/// Get the password hash for a document by ID. Returns None if no hash set.
pub fn get_password_hash(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
) -> Result<Option<String>> {
    let sql = format!("SELECT _password_hash FROM {} WHERE id = ?1", slug);

    let result = conn.query_row(&sql, [id], |row| {
        row.get::<_, Option<String>>(0)
    });

    match result {
        Ok(hash) => Ok(hash),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to get password hash for {} in {}", id, slug)),
    }
}

/// Update the password hash for a document by ID.
/// Hashes the plaintext password before storing.
pub fn update_password(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    password: &str,
) -> Result<()> {
    let hash = crate::core::auth::hash_password(password)?;
    let sql = format!("UPDATE {} SET _password_hash = ?1 WHERE id = ?2", slug);
    conn.execute(&sql, rusqlite::params![hash, id])
        .with_context(|| format!("Failed to update password for {} in {}", id, slug))?;
    Ok(())
}

// ── Reset token functions ─────────────────────────────────────────────────

/// Store a password reset token and expiry for a user.
pub fn set_reset_token(
    conn: &rusqlite::Connection,
    slug: &str,
    user_id: &str,
    token: &str,
    exp: i64,
) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _reset_token = ?1, _reset_token_exp = ?2 WHERE id = ?3",
        slug
    );
    conn.execute(&sql, rusqlite::params![token, exp, user_id])
        .with_context(|| format!("Failed to set reset token for {} in {}", user_id, slug))?;
    Ok(())
}

/// Find a user by their reset token. Returns the document and token expiry.
pub fn find_by_reset_token(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<(Document, i64)>> {
    let column_names = get_column_names(def);
    let cols = column_names.join(", ");
    let sql = format!(
        "SELECT {}, _reset_token_exp FROM {} WHERE _reset_token = ?1",
        cols, slug
    );

    let result = conn.query_row(&sql, [token], |row| {
        let doc = row_to_document(row, &column_names)?;
        let exp: i64 = row.get(column_names.len())?;
        Ok((doc, exp))
    });

    match result {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to find user by reset token in {}", slug)),
    }
}

/// Clear the reset token for a user (after successful reset or expiry).
pub fn clear_reset_token(
    conn: &rusqlite::Connection,
    slug: &str,
    user_id: &str,
) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _reset_token = NULL, _reset_token_exp = NULL WHERE id = ?1",
        slug
    );
    conn.execute(&sql, [user_id])
        .with_context(|| format!("Failed to clear reset token for {} in {}", user_id, slug))?;
    Ok(())
}

// ── Email verification functions ──────────────────────────────────────────

/// Store a verification token for a user.
pub fn set_verification_token(
    conn: &rusqlite::Connection,
    slug: &str,
    user_id: &str,
    token: &str,
) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _verification_token = ?1 WHERE id = ?2",
        slug
    );
    conn.execute(&sql, rusqlite::params![token, user_id])
        .with_context(|| format!("Failed to set verification token for {} in {}", user_id, slug))?;
    Ok(())
}

/// Find a user by their verification token.
pub fn find_by_verification_token(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    token: &str,
) -> Result<Option<Document>> {
    let column_names = get_column_names(def);
    let sql = format!(
        "SELECT {} FROM {} WHERE _verification_token = ?1",
        column_names.join(", "), slug
    );

    let result = conn.query_row(&sql, [token], |row| {
        row_to_document(row, &column_names)
    });

    match result {
        Ok(doc) => Ok(Some(doc)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context(format!("Failed to find user by verification token in {}", slug)),
    }
}

/// Mark a user as verified (set _verified = 1, clear token).
pub fn mark_verified(
    conn: &rusqlite::Connection,
    slug: &str,
    user_id: &str,
) -> Result<()> {
    let sql = format!(
        "UPDATE {} SET _verified = 1, _verification_token = NULL WHERE id = ?1",
        slug
    );
    conn.execute(&sql, [user_id])
        .with_context(|| format!("Failed to mark user {} as verified in {}", user_id, slug))?;
    Ok(())
}

/// Check if a user is verified.
pub fn is_verified(
    conn: &rusqlite::Connection,
    slug: &str,
    user_id: &str,
) -> Result<bool> {
    let sql = format!("SELECT _verified FROM {} WHERE id = ?1", slug);
    let result = conn.query_row(&sql, [user_id], |row| {
        row.get::<_, Option<i64>>(0)
    });
    match result {
        Ok(Some(v)) => Ok(v != 0),
        Ok(None) => Ok(false),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(e).context(format!("Failed to check verification for {} in {}", user_id, slug)),
    }
}

pub fn get_column_names(def: &CollectionDefinition) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                names.push(format!("{}__{}", field.name, sub.name));
            }
        } else if field.has_parent_column() {
            names.push(field.name.clone());
        }
    }
    if def.timestamps {
        names.push("created_at".to_string());
        names.push("updated_at".to_string());
    }
    names
}

/// Get locale-aware SELECT expressions and result column names for a collection.
/// Returns (select_exprs, result_names) where:
/// - select_exprs: SQL expressions for the SELECT clause (may include aliases/COALESCE)
/// - result_names: column names in the result set (used by row_to_document)
pub fn get_locale_select_columns(
    fields: &[FieldDefinition],
    timestamps: bool,
    locale_ctx: &LocaleContext,
) -> (Vec<String>, Vec<String>) {
    let mut select_exprs = vec!["id".to_string()];
    let mut result_names = vec!["id".to_string()];

    for field in fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                let base = format!("{}__{}", field.name, sub.name);
                let is_localized = (field.localized || sub.localized) && locale_ctx.config.is_enabled();
                if is_localized {
                    add_locale_columns(&mut select_exprs, &mut result_names, &base, locale_ctx);
                } else {
                    select_exprs.push(base.clone());
                    result_names.push(base);
                }
            }
        } else if field.has_parent_column() {
            if field.localized && locale_ctx.config.is_enabled() {
                add_locale_columns(&mut select_exprs, &mut result_names, &field.name, locale_ctx);
            } else {
                select_exprs.push(field.name.clone());
                result_names.push(field.name.clone());
            }
        }
    }

    if timestamps {
        select_exprs.push("created_at".to_string());
        result_names.push("created_at".to_string());
        select_exprs.push("updated_at".to_string());
        result_names.push("updated_at".to_string());
    }

    (select_exprs, result_names)
}

/// Add SELECT expressions for a localized field based on the locale mode.
fn add_locale_columns(
    select_exprs: &mut Vec<String>,
    result_names: &mut Vec<String>,
    field_name: &str,
    locale_ctx: &LocaleContext,
) {
    match &locale_ctx.mode {
        LocaleMode::Default => {
            let locale = &locale_ctx.config.default_locale;
            // No fallback needed for default locale
            select_exprs.push(format!("{}__{} AS {}", field_name, locale, field_name));
            result_names.push(field_name.to_string());
        }
        LocaleMode::Single(locale) => {
            if locale_ctx.config.fallback && *locale != locale_ctx.config.default_locale {
                select_exprs.push(format!(
                    "COALESCE({}__{}, {}__{}) AS {}",
                    field_name, locale,
                    field_name, locale_ctx.config.default_locale,
                    field_name
                ));
            } else {
                select_exprs.push(format!("{}__{} AS {}", field_name, locale, field_name));
            }
            result_names.push(field_name.to_string());
        }
        LocaleMode::All => {
            // Select all locale columns — no alias, keep suffixed names
            for locale in &locale_ctx.config.locales {
                let col = format!("{}__{}", field_name, locale);
                select_exprs.push(col.clone());
                result_names.push(col);
            }
        }
    }
}

/// Resolve filter clauses to use locale-specific column names.
fn resolve_filters(filters: &[FilterClause], def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> Vec<FilterClause> {
    filters.iter().map(|clause| {
        match clause {
            FilterClause::Single(f) => {
                let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                FilterClause::Single(Filter { field: resolved, op: f.op.clone() })
            }
            FilterClause::Or(groups) => {
                FilterClause::Or(groups.iter().map(|group| {
                    group.iter().map(|f| {
                        let resolved = resolve_filter_column(&f.field, def, locale_ctx);
                        Filter { field: resolved, op: f.op.clone() }
                    }).collect()
                }).collect())
            }
        }
    }).collect()
}

/// Group locale-suffixed fields into nested objects for `LocaleMode::All`.
/// Converts `title__en: "Hello", title__de: "Hallo"` into `title: { en: "Hello", de: "Hallo" }`.
fn group_locale_fields(doc: &mut Document, fields: &[FieldDefinition], locale_config: &LocaleConfig) {
    for field in fields {
        if field.field_type == FieldType::Group {
            // For groups, each sub-field might be localized
            // The group is already reconstructed by hydrate_document, so we handle it there.
            // For All mode, we need to group sub-fields within the group.
            for sub in &field.fields {
                if (field.localized || sub.localized) && locale_config.is_enabled() {
                    let base = format!("{}__{}", field.name, sub.name);
                    let mut locale_map = serde_json::Map::new();
                    for locale in &locale_config.locales {
                        let col = format!("{}__{}", base, locale);
                        if let Some(val) = doc.fields.remove(&col) {
                            locale_map.insert(locale.clone(), val);
                        }
                    }
                    if !locale_map.is_empty() {
                        doc.fields.insert(base, serde_json::Value::Object(locale_map));
                    }
                }
            }
        } else if field.has_parent_column() && field.localized && locale_config.is_enabled() {
            let mut locale_map = serde_json::Map::new();
            for locale in &locale_config.locales {
                let col = format!("{}__{}", field.name, locale);
                if let Some(val) = doc.fields.remove(&col) {
                    locale_map.insert(locale.clone(), val);
                }
            }
            if !locale_map.is_empty() {
                doc.fields.insert(field.name.clone(), serde_json::Value::Object(locale_map));
            }
        }
    }
}

/// Map a flat field name to the actual locale-suffixed column name for writes.
fn locale_write_column(field_name: &str, field: &FieldDefinition, locale_ctx: &Option<&LocaleContext>) -> String {
    if let Some(ctx) = locale_ctx {
        if field.localized && ctx.config.is_enabled() {
            let locale = match &ctx.mode {
                LocaleMode::Single(l) => l.as_str(),
                _ => ctx.config.default_locale.as_str(),
            };
            return format!("{}__{}", field_name, locale);
        }
    }
    field_name.to_string()
}

/// Get the set of valid filter column names, accounting for locale.
/// Localized fields map their undecorated names to the locale-specific column.
fn get_valid_filter_columns(def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> HashSet<String> {
    let mut valid = HashSet::new();
    valid.insert("id".to_string());
    for field in &def.fields {
        if field.field_type == FieldType::Group {
            for sub in &field.fields {
                valid.insert(format!("{}__{}", field.name, sub.name));
            }
        } else if field.has_parent_column() {
            valid.insert(field.name.clone());
        }
    }
    if def.timestamps {
        valid.insert("created_at".to_string());
        valid.insert("updated_at".to_string());
    }
    let _ = locale_ctx; // filter validation uses undecorated field names
    valid
}

/// Map a filter field name to its actual column name in SQL, accounting for locale.
fn resolve_filter_column(field_name: &str, def: &CollectionDefinition, locale_ctx: Option<&LocaleContext>) -> String {
    if let Some(ctx) = locale_ctx {
        if ctx.config.is_enabled() {
            // Check if this field is localized
            for field in &def.fields {
                if field.field_type == FieldType::Group {
                    let prefix = format!("{}__{}", field.name, "");
                    if field_name.starts_with(&prefix) {
                        let sub_name = &field_name[prefix.len()..];
                        for sub in &field.fields {
                            if sub.name == sub_name && (field.localized || sub.localized) {
                                let locale = match &ctx.mode {
                                    LocaleMode::Single(l) => l.as_str(),
                                    _ => ctx.config.default_locale.as_str(),
                                };
                                return format!("{}__{}", field_name, locale);
                            }
                        }
                    }
                } else if field.name == field_name && field.localized {
                    let locale = match &ctx.mode {
                        LocaleMode::Single(l) => l.as_str(),
                        _ => ctx.config.default_locale.as_str(),
                    };
                    return format!("{}__{}", field_name, locale);
                }
            }
        }
    }
    field_name.to_string()
}

// ── Join table functions (has-many relationships + arrays) ────────────────────

/// Set related IDs for a has-many relationship junction table.
/// Deletes all existing rows for the parent and inserts new ones with _order.
pub fn set_related_ids(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
    ids: &[String],
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field);
    conn.execute(
        &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
        [parent_id],
    ).with_context(|| format!("Failed to clear junction table {}", table_name))?;

    let sql = format!(
        "INSERT INTO {} (parent_id, related_id, _order) VALUES (?1, ?2, ?3)",
        table_name
    );
    let mut stmt = conn.prepare(&sql)?;
    for (i, id) in ids.iter().enumerate() {
        stmt.execute(rusqlite::params![parent_id, id, i as i64])?;
    }
    Ok(())
}

/// Find related IDs for a has-many relationship junction table, ordered.
pub fn find_related_ids(
    conn: &rusqlite::Connection,
    collection: &str,
    field: &str,
    parent_id: &str,
) -> Result<Vec<String>> {
    let table_name = format!("{}_{}", collection, field);
    let sql = format!(
        "SELECT related_id FROM {} WHERE parent_id = ?1 ORDER BY _order",
        table_name
    );
    let mut stmt = conn.prepare(&sql)?;
    let ids: Vec<String> = stmt.query_map([parent_id], |row| {
        row.get::<_, String>(0)
    })?.filter_map(|r| r.ok()).collect();
    Ok(ids)
}

/// Set array rows for an array field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
pub fn set_array_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[HashMap<String, String>],
    sub_fields: &[crate::core::field::FieldDefinition],
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field_name);
    conn.execute(
        &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
        [parent_id],
    ).with_context(|| format!("Failed to clear array table {}", table_name))?;

    if rows.is_empty() || sub_fields.is_empty() {
        return Ok(());
    }

    // Build column list from sub-fields
    let col_names: Vec<&str> = sub_fields.iter().map(|f| f.name.as_str()).collect();
    let all_cols = format!(
        "id, parent_id, _order, {}",
        col_names.join(", ")
    );
    let placeholders = format!(
        "?1, ?2, ?3, {}",
        (4..4 + col_names.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(", ")
    );
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", table_name, all_cols, placeholders);

    let mut stmt = conn.prepare(&sql)?;
    for (order, row) in rows.iter().enumerate() {
        let id = nanoid::nanoid!();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(id),
            Box::new(parent_id.to_string()),
            Box::new(order as i64),
        ];
        for sf in sub_fields {
            let value = row.get(&sf.name).cloned().unwrap_or_default();
            params.push(coerce_value(&sf.field_type, &value));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
        stmt.execute(rusqlite::params_from_iter(param_refs.iter()))?;
    }
    Ok(())
}

/// Find array rows for an array field join table, ordered.
pub fn find_array_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    sub_fields: &[crate::core::field::FieldDefinition],
) -> Result<Vec<serde_json::Value>> {
    let table_name = format!("{}_{}", collection, field_name);
    let col_names: Vec<&str> = sub_fields.iter().map(|f| f.name.as_str()).collect();
    let select_cols = if col_names.is_empty() {
        "id".to_string()
    } else {
        format!("id, {}", col_names.join(", "))
    };
    let sql = format!(
        "SELECT {} FROM {} WHERE parent_id = ?1 ORDER BY _order",
        select_cols, table_name
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([parent_id], |row| {
        let mut map = serde_json::Map::new();
        let id: String = row.get(0)?;
        map.insert("id".to_string(), serde_json::Value::String(id));
        for (i, sf) in sub_fields.iter().enumerate() {
            let val: rusqlite::types::Value = row.get(i + 1)?;
            let json_val = match val {
                rusqlite::types::Value::Null => serde_json::Value::Null,
                rusqlite::types::Value::Integer(n) => serde_json::json!(n),
                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
                rusqlite::types::Value::Blob(_) => serde_json::Value::Null,
            };
            map.insert(sf.name.clone(), json_val);
        }
        Ok(serde_json::Value::Object(map))
    })?.filter_map(|r| r.ok()).collect();
    Ok(rows)
}

/// Set block rows for a blocks field join table.
/// Deletes all existing rows for the parent and inserts new ones with nanoid + _order.
pub fn set_block_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
    rows: &[serde_json::Value],
) -> Result<()> {
    let table_name = format!("{}_{}", collection, field_name);
    conn.execute(
        &format!("DELETE FROM {} WHERE parent_id = ?1", table_name),
        [parent_id],
    ).with_context(|| format!("Failed to clear blocks table {}", table_name))?;

    if rows.is_empty() {
        return Ok(());
    }

    let sql = format!(
        "INSERT INTO {} (id, parent_id, _order, _block_type, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        table_name
    );
    let mut stmt = conn.prepare(&sql)?;
    for (order, row) in rows.iter().enumerate() {
        let id = nanoid::nanoid!();
        let block_type = row.get("_block_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        // Store everything except _block_type as JSON data
        let mut data_map = match row.as_object() {
            Some(m) => m.clone(),
            None => serde_json::Map::new(),
        };
        data_map.remove("_block_type");
        data_map.remove("id");
        let data_json = serde_json::Value::Object(data_map).to_string();
        stmt.execute(rusqlite::params![id, parent_id, order as i64, block_type, data_json])?;
    }
    Ok(())
}

/// Find block rows for a blocks field join table, ordered.
pub fn find_block_rows(
    conn: &rusqlite::Connection,
    collection: &str,
    field_name: &str,
    parent_id: &str,
) -> Result<Vec<serde_json::Value>> {
    let table_name = format!("{}_{}", collection, field_name);
    let sql = format!(
        "SELECT id, _block_type, data FROM {} WHERE parent_id = ?1 ORDER BY _order",
        table_name
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([parent_id], |row| {
        let id: String = row.get(0)?;
        let block_type: String = row.get(1)?;
        let data_json: String = row.get(2)?;
        Ok((id, block_type, data_json))
    })?.filter_map(|r| r.ok()).map(|(id, block_type, data_json)| {
        let mut map = match serde_json::from_str::<serde_json::Value>(&data_json) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        map.insert("id".to_string(), serde_json::Value::String(id));
        map.insert("_block_type".to_string(), serde_json::Value::String(block_type));
        serde_json::Value::Object(map)
    }).collect();
    Ok(rows)
}

/// Hydrate a document with join table data (has-many relationships and arrays).
/// Populates `doc.fields` with JSON arrays for each join-table field.
/// If `select` is provided, skip hydrating fields not in the select list.
pub fn hydrate_document(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    select: Option<&[String]>,
) -> Result<()> {
    for field in &def.fields {
        // Skip hydrating fields not in the select list
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        let ids = find_related_ids(conn, slug, &field.name, &doc.id)?;
                        let json_ids: Vec<serde_json::Value> = ids.into_iter()
                            .map(serde_json::Value::String)
                            .collect();
                        doc.fields.insert(field.name.clone(), serde_json::Value::Array(json_ids));
                    }
                }
            }
            FieldType::Array => {
                let rows = find_array_rows(conn, slug, &field.name, &doc.id, &field.fields)?;
                doc.fields.insert(field.name.clone(), serde_json::Value::Array(rows));
            }
            FieldType::Group => {
                // Reconstruct nested object from prefixed columns: seo__title → { seo: { title: val } }
                let mut group_obj = serde_json::Map::new();
                for sub in &field.fields {
                    let col_name = format!("{}__{}", field.name, sub.name);
                    if let Some(val) = doc.fields.remove(&col_name) {
                        group_obj.insert(sub.name.clone(), val);
                    }
                }
                doc.fields.insert(field.name.clone(), serde_json::Value::Object(group_obj));
            }
            FieldType::Blocks => {
                let rows = find_block_rows(conn, slug, &field.name, &doc.id)?;
                doc.fields.insert(field.name.clone(), serde_json::Value::Array(rows));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Convert a Document into a serde_json::Value for embedding in a parent's fields.
fn document_to_json(doc: &Document, collection: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
    map.insert("collection".to_string(), serde_json::Value::String(collection.to_string()));
    for (k, v) in &doc.fields {
        map.insert(k.clone(), v.clone());
    }
    if let Some(ref ts) = doc.created_at {
        map.insert("created_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    if let Some(ref ts) = doc.updated_at {
        map.insert("updated_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    serde_json::Value::Object(map)
}

/// Recursively populate relationship fields with full document objects.
/// depth=0 is a no-op. Tracks visited (collection, id) pairs to break cycles.
/// If `select` is provided, only populate relationship fields in the select list.
/// Recursive calls for nested docs always pass `None` (populate all nested fields).
pub fn populate_relationships(
    conn: &rusqlite::Connection,
    registry: &crate::core::Registry,
    collection_slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    depth: i32,
    visited: &mut HashSet<(String, String)>,
    select: Option<&[String]>,
) -> Result<()> {
    if depth <= 0 {
        return Ok(());
    }

    let visit_key = (collection_slug.to_string(), doc.id.clone());
    if visited.contains(&visit_key) {
        return Ok(());
    }
    visited.insert(visit_key);

    for field in &def.fields {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }
        // Skip populating fields not in the select list
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        // Field-level max_depth caps the effective depth for this field
        let effective_depth = match rel.max_depth {
            Some(max) if max < depth => max,
            _ => depth,
        };
        if effective_depth <= 0 {
            continue;
        }

        let rel_def = match registry.get_collection(&rel.collection) {
            Some(d) => d.clone(),
            None => continue,
        };

        if rel.has_many {
            // Has-many: doc.fields[name] is already a JSON array of ID strings (from hydration)
            let ids: Vec<String> = match doc.fields.get(&field.name) {
                Some(serde_json::Value::Array(arr)) => {
                    arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                }
                _ => continue,
            };

            let mut populated = Vec::new();
            for id in &ids {
                if visited.contains(&(rel.collection.clone(), id.clone())) {
                    // Already visited — keep as ID string
                    populated.push(serde_json::Value::String(id.clone()));
                    continue;
                }
                match find_by_id(conn, &rel.collection, &rel_def, id, None)? {
                    Some(mut related_doc) => {
                        hydrate_document(conn, &rel.collection, &rel_def, &mut related_doc, None)?;
                        if let Some(ref uc) = rel_def.upload {
                            if uc.enabled {
                                crate::core::upload::assemble_sizes_object(&mut related_doc, uc);
                            }
                        }
                        populate_relationships(
                            conn, registry, &rel.collection, &rel_def,
                            &mut related_doc, effective_depth - 1, visited, None,
                        )?;
                        populated.push(document_to_json(&related_doc, &rel.collection));
                    }
                    None => {
                        populated.push(serde_json::Value::String(id.clone()));
                    }
                }
            }
            doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
        } else {
            // Has-one: doc.fields[name] is a string ID
            let id = match doc.fields.get(&field.name) {
                Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
                _ => continue,
            };

            if visited.contains(&(rel.collection.clone(), id.clone())) {
                continue; // Already visited — keep as ID string
            }

            match find_by_id(conn, &rel.collection, &rel_def, &id, None)? {
                Some(mut related_doc) => {
                    hydrate_document(conn, &rel.collection, &rel_def, &mut related_doc, None)?;
                    if let Some(ref uc) = rel_def.upload {
                        if uc.enabled {
                            crate::core::upload::assemble_sizes_object(&mut related_doc, uc);
                        }
                    }
                    populate_relationships(
                        conn, registry, &rel.collection, &rel_def,
                        &mut related_doc, effective_depth - 1, visited, None,
                    )?;
                    doc.fields.insert(field.name.clone(), document_to_json(&related_doc, &rel.collection));
                }
                None => {} // ID not found — keep as-is
            }
        }
    }

    Ok(())
}

/// Save join table data for has-many relationships and arrays.
/// Extracts relevant data from the data map and writes to join tables.
pub fn save_join_table_data(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    parent_id: &str,
    data: &HashMap<String, serde_json::Value>,
) -> Result<()> {
    for field in &def.fields {
        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        // Only touch join table if the field was explicitly included in the data.
                        // Absent = don't modify (supports partial updates).
                        if let Some(val) = data.get(&field.name) {
                            let ids = match val {
                                serde_json::Value::Array(arr) => {
                                    arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                                }
                                serde_json::Value::String(s) => {
                                    // Comma-separated IDs from form data
                                    if s.is_empty() {
                                        Vec::new()
                                    } else {
                                        s.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                                    }
                                }
                                _ => Vec::new(),
                            };
                            set_related_ids(conn, slug, &field.name, parent_id, &ids)?;
                        }
                    }
                }
            }
            FieldType::Array => {
                // Only touch join table if the field was explicitly included in the data.
                // Absent = don't modify (supports partial updates).
                if let Some(val) = data.get(&field.name) {
                    let rows = match val {
                        serde_json::Value::Array(arr) => {
                            arr.iter().filter_map(|v| {
                                if let serde_json::Value::Object(map) = v {
                                    let row: HashMap<String, String> = map.iter().map(|(k, v)| {
                                        let s = match v {
                                            serde_json::Value::String(s) => s.clone(),
                                            other => other.to_string(),
                                        };
                                        (k.clone(), s)
                                    }).collect();
                                    Some(row)
                                } else {
                                    None
                                }
                            }).collect()
                        }
                        _ => Vec::new(),
                    };
                    set_array_rows(conn, slug, &field.name, parent_id, &rows, &field.fields)?;
                }
            }
            FieldType::Blocks => {
                if let Some(val) = data.get(&field.name) {
                    let rows = match val {
                        serde_json::Value::Array(arr) => arr.clone(),
                        _ => Vec::new(),
                    };
                    set_block_rows(conn, slug, &field.name, parent_id, &rows)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_identifier_accepts_valid() {
        assert!(is_valid_identifier("title"));
        assert!(is_valid_identifier("created_at"));
        assert!(is_valid_identifier("field_123"));
        assert!(is_valid_identifier("id"));
    }

    #[test]
    fn is_valid_identifier_rejects_invalid() {
        assert!(!is_valid_identifier(""));
        assert!(!is_valid_identifier("field name"));
        assert!(!is_valid_identifier("1=1; DROP TABLE posts; --"));
        assert!(!is_valid_identifier("field-name"));
        assert!(!is_valid_identifier("field.name"));
        assert!(!is_valid_identifier("field;name"));
    }

    #[test]
    fn validate_field_name_accepts_known() {
        let valid: HashSet<String> = ["id", "title", "status"]
            .iter().map(|s| s.to_string()).collect();
        assert!(validate_field_name("title", &valid).is_ok());
        assert!(validate_field_name("id", &valid).is_ok());
    }

    #[test]
    fn validate_field_name_rejects_unknown() {
        let valid: HashSet<String> = ["id", "title", "status"]
            .iter().map(|s| s.to_string()).collect();
        let err = validate_field_name("nonexistent", &valid).unwrap_err();
        assert!(err.to_string().contains("Invalid field 'nonexistent'"));
    }

    // ── LocaleContext tests ──────────────────────────────────────────────────

    #[test]
    fn locale_context_disabled() {
        let config = crate::config::LocaleConfig::default(); // no locales
        let ctx = LocaleContext::from_locale_string(None, &config);
        assert!(ctx.is_none(), "Should be None when localization is disabled");
    }

    #[test]
    fn locale_context_all() {
        let config = crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let ctx = LocaleContext::from_locale_string(Some("all"), &config);
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::All));
    }

    #[test]
    fn locale_context_specific() {
        let config = crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let ctx = LocaleContext::from_locale_string(Some("de"), &config);
        assert!(ctx.is_some());
        match ctx.unwrap().mode {
            LocaleMode::Single(locale) => assert_eq!(locale, "de"),
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn locale_context_default() {
        let config = crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let ctx = LocaleContext::from_locale_string(None, &config);
        assert!(ctx.is_some());
        assert!(matches!(ctx.unwrap().mode, LocaleMode::Default));
    }
}
