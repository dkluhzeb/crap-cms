use anyhow::{Context, Result, bail};
use rusqlite::params_from_iter;
use std::collections::HashMap;
use std::collections::HashSet;

use crate::core::{CollectionDefinition, Document};
use crate::core::collection::GlobalDefinition;
use crate::core::field::FieldType;
use super::document::row_to_document;

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

#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub op: FilterOp,
}

#[derive(Debug, Clone)]
pub enum FilterClause {
    Single(Filter),
    Or(Vec<Filter>),
}

#[derive(Debug, Default, Clone)]
pub struct FindQuery {
    pub filters: Vec<FilterClause>,
    pub order_by: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
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
pub fn validate_query_fields(def: &CollectionDefinition, query: &FindQuery) -> Result<()> {
    let valid: HashSet<String> = get_column_names(def).into_iter().collect();

    for clause in &query.filters {
        match clause {
            FilterClause::Single(f) => validate_field_name(&f.field, &valid)?,
            FilterClause::Or(filters) => {
                for f in filters {
                    validate_field_name(&f.field, &valid)?;
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
pub fn find(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, query: &FindQuery) -> Result<Vec<Document>> {
    validate_query_fields(def, query)?;
    let column_names = get_column_names(def);

    let mut sql = format!("SELECT {} FROM {}", column_names.join(", "), slug);
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let where_clause = build_where_clause(&query.filters, &mut params)?;
    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
    }

    if let Some(ref order) = query.order_by {
        // Parse "-field" as "field DESC"
        let (col, dir) = if let Some(stripped) = order.strip_prefix('-') {
            (stripped, "DESC")
        } else {
            (order.as_str(), "ASC")
        };
        sql.push_str(&format!(" ORDER BY {} {}", col, dir));
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
        row_to_document(row, &column_names)
    }).with_context(|| format!("Failed to execute query on '{}'", slug))?;

    let mut documents = Vec::new();
    for row in rows {
        documents.push(row?);
    }

    Ok(documents)
}

/// Find a single document by ID.
pub fn find_by_id(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, id: &str) -> Result<Option<Document>> {
    let column_names = get_column_names(def);

    let sql = format!("SELECT {} FROM {} WHERE id = ?1", column_names.join(", "), slug);

    let result = conn.query_row(&sql, [id], |row| {
        row_to_document(row, &column_names)
    });

    match result {
        Ok(doc) => Ok(Some(doc)),
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
) -> Result<Document> {
    let id = nanoid::nanoid!();
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut columns = vec!["id".to_string()];
    let mut placeholders = vec!["?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(id.clone())];
    let mut idx = 2;

    for field in &def.fields {
        if !field.has_parent_column() {
            continue;
        }
        if let Some(value) = data.get(&field.name) {
            columns.push(field.name.clone());
            placeholders.push(format!("?{}", idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            // Absent checkbox = false
            columns.push(field.name.clone());
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

    // Return the created document (same connection — no second pool.get())
    find_by_id(conn, slug, def, &id)?
        .ok_or_else(|| anyhow::anyhow!("Failed to find newly created document"))
}

/// Update a document by ID. Returns the updated document.
pub fn update(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    id: &str,
    data: &HashMap<String, String>,
) -> Result<Document> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    for field in &def.fields {
        if !field.has_parent_column() {
            continue;
        }
        if let Some(value) = data.get(&field.name) {
            set_clauses.push(format!("{} = ?{}", field.name, idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            set_clauses.push(format!("{} = ?{}", field.name, idx));
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
        return find_by_id(conn, slug, def, id)?
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

    // Return the updated document (same connection — no second pool.get())
    find_by_id(conn, slug, def, id)?
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
pub fn count(conn: &rusqlite::Connection, slug: &str, def: &CollectionDefinition, filters: &[FilterClause]) -> Result<i64> {
    // Validate filter fields
    let valid: HashSet<String> = get_column_names(def).into_iter().collect();
    for clause in filters {
        match clause {
            FilterClause::Single(f) => validate_field_name(&f.field, &valid)?,
            FilterClause::Or(fs) => {
                for f in fs {
                    validate_field_name(&f.field, &valid)?;
                }
            }
        }
    }

    let mut sql = format!("SELECT COUNT(*) FROM {}", slug);
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    let where_clause = build_where_clause(filters, &mut params)?;
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
pub fn get_global(conn: &rusqlite::Connection, slug: &str, def: &GlobalDefinition) -> Result<Document> {
    let table_name = format!("_global_{}", slug);
    let column_names = get_global_column_names(def);

    let sql = format!("SELECT {} FROM {} WHERE id = 'default'", column_names.join(", "), table_name);

    conn.query_row(&sql, [], |row| {
        row_to_document(row, &column_names)
    }).with_context(|| format!("Failed to get global '{}'", slug))
}

/// Update the single global document in `_global_{slug}`. Returns the updated document.
pub fn update_global(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &GlobalDefinition,
    data: &HashMap<String, String>,
) -> Result<Document> {
    let table_name = format!("_global_{}", slug);
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    for field in &def.fields {
        if let Some(value) = data.get(&field.name) {
            set_clauses.push(format!("{} = ?{}", field.name, idx));
            params.push(coerce_value(&field.field_type, value));
            idx += 1;
        } else if field.field_type == FieldType::Checkbox {
            set_clauses.push(format!("{} = ?{}", field.name, idx));
            params.push(Box::new(0i32));
            idx += 1;
        }
    }

    // Globals always have timestamps (migration always creates them)
    set_clauses.push(format!("updated_at = ?{}", idx));
    params.push(Box::new(now));

    if set_clauses.is_empty() {
        return get_global(conn, slug, def);
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = 'default'",
        table_name,
        set_clauses.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to update global '{}'", slug))?;

    get_global(conn, slug, def)
}

fn get_global_column_names(def: &GlobalDefinition) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    for field in &def.fields {
        names.push(field.name.clone());
    }
    // Globals always have timestamps
    names.push("created_at".to_string());
    names.push("updated_at".to_string());
    names
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
            params.push(Box::new(format!("%{}%", v)));
            format!("{} LIKE ?", f.field)
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
            FilterClause::Or(filters) => {
                if filters.len() == 1 {
                    conditions.push(build_filter_condition(&filters[0], params)?);
                } else {
                    let mut or_parts = Vec::new();
                    for f in filters {
                        or_parts.push(build_filter_condition(f, params)?);
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

pub fn get_column_names(def: &CollectionDefinition) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    for field in &def.fields {
        if field.has_parent_column() {
            names.push(field.name.clone());
        }
    }
    if def.timestamps {
        names.push("created_at".to_string());
        names.push("updated_at".to_string());
    }
    names
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

/// Hydrate a document with join table data (has-many relationships and arrays).
/// Populates `doc.fields` with JSON arrays for each join-table field.
pub fn hydrate_document(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
) -> Result<()> {
    for field in &def.fields {
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
pub fn populate_relationships(
    conn: &rusqlite::Connection,
    registry: &crate::core::Registry,
    collection_slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    depth: i32,
    visited: &mut HashSet<(String, String)>,
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
        if field.field_type != FieldType::Relationship {
            continue;
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
                match find_by_id(conn, &rel.collection, &rel_def, id)? {
                    Some(mut related_doc) => {
                        hydrate_document(conn, &rel.collection, &rel_def, &mut related_doc)?;
                        populate_relationships(
                            conn, registry, &rel.collection, &rel_def,
                            &mut related_doc, effective_depth - 1, visited,
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

            match find_by_id(conn, &rel.collection, &rel_def, &id)? {
                Some(mut related_doc) => {
                    hydrate_document(conn, &rel.collection, &rel_def, &mut related_doc)?;
                    populate_relationships(
                        conn, registry, &rel.collection, &rel_def,
                        &mut related_doc, effective_depth - 1, visited,
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
}
