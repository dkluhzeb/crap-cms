//! CRUD tool implementations: find, find_by_id, create, update, delete, and global ops.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context as _, Result};
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use crate::{
    config::CrapConfig,
    core::{Document, Registry},
    db::{DbPool, FindQuery, query},
    hooks::HookRunner,
    service::{
        ReadOptions, RunnerReadHooks, WriteInput, create_document, delete_document,
        find_document_by_id, find_documents, get_global_document, update_document,
        update_global_document,
    },
};

/// Parse JSON `where` object into filter clauses.
/// Supports `{ field: "value" }` (equals) and `{ field: { op: value } }` (operator-based).
pub(super) fn parse_where_filters(args: &Value) -> Vec<query::FilterClause> {
    let Some(where_obj) = args.get("where").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut clauses = Vec::new();

    for (field, value) in where_obj {
        match value {
            Value::String(s) => {
                clauses.push(make_equals_clause(field, s.clone()));
            }
            Value::Number(n) => {
                clauses.push(make_equals_clause(field, n.to_string()));
            }
            Value::Bool(b) => {
                clauses.push(make_equals_clause(field, bool_to_string(*b)));
            }
            Value::Object(ops) => {
                parse_operator_filters(field, ops, &mut clauses);
            }
            _ => {}
        }
    }

    clauses
}

/// Create an Equals filter clause for a field.
fn make_equals_clause(field: &str, value: String) -> query::FilterClause {
    query::FilterClause::Single(query::Filter {
        field: field.to_string(),
        op: query::FilterOp::Equals(value),
    })
}

/// Parse operator-based filters: `{ "greater_than": "50", "less_than": "100" }`.
fn parse_operator_filters(
    field: &str,
    ops: &Map<String, Value>,
    clauses: &mut Vec<query::FilterClause>,
) {
    for (op_name, op_value) in ops {
        match op_name.as_str() {
            "in" | "not_in" => {
                let Some(arr) = op_value.as_array() else {
                    continue;
                };
                let vals: Vec<String> = arr
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect();
                let op = if op_name == "in" {
                    query::FilterOp::In(vals)
                } else {
                    query::FilterOp::NotIn(vals)
                };
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op,
                }));
            }
            "exists" => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op: query::FilterOp::Exists,
                }));
            }
            "not_exists" => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op: query::FilterOp::NotExists,
                }));
            }
            _ => {
                let val_str = match op_value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => bool_to_string(*b),
                    _ => continue,
                };
                let Some(op) = parse_scalar_op(op_name, val_str) else {
                    continue;
                };
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.to_string(),
                    op,
                }));
            }
        }
    }
}

/// Parse a scalar filter operator name into a FilterOp.
fn parse_scalar_op(op_name: &str, val: String) -> Option<query::FilterOp> {
    match op_name {
        "equals" => Some(query::FilterOp::Equals(val)),
        "not_equals" => Some(query::FilterOp::NotEquals(val)),
        "contains" => Some(query::FilterOp::Contains(val)),
        "greater_than" => Some(query::FilterOp::GreaterThan(val)),
        "greater_than_equal" | "greater_than_or_equal" => {
            Some(query::FilterOp::GreaterThanOrEqual(val))
        }
        "less_than" => Some(query::FilterOp::LessThan(val)),
        "less_than_equal" | "less_than_or_equal" => Some(query::FilterOp::LessThanOrEqual(val)),
        "like" => Some(query::FilterOp::Like(val)),
        unknown => {
            warn!("Unknown MCP filter operator '{}', skipping", unknown);

            None
        }
    }
}

fn bool_to_string(b: bool) -> String {
    if b { "1" } else { "0" }.to_string()
}

/// Convert a Document to a JSON Value.
pub(super) fn doc_to_json(doc: &Document) -> Value {
    let mut obj = Map::new();
    obj.insert("id".to_string(), Value::String(doc.id.to_string()));
    for (k, v) in &doc.fields {
        obj.insert(k.clone(), v.clone());
    }
    if let Some(ref ca) = doc.created_at {
        obj.insert("created_at".to_string(), Value::String(ca.clone()));
    }
    if let Some(ref ua) = doc.updated_at {
        obj.insert("updated_at".to_string(), Value::String(ua.clone()));
    }
    Value::Object(obj)
}

/// Extract flat string data and join data (arrays/objects) from JSON args.
fn extract_data_from_args(
    args: &Value,
    skip_keys: &[&str],
) -> (HashMap<String, String>, HashMap<String, Value>) {
    let mut data = HashMap::new();
    let mut join_data = HashMap::new();

    let Some(obj) = args.as_object() else {
        return (data, join_data);
    };

    for (k, v) in obj {
        if skip_keys.contains(&k.as_str()) {
            continue;
        }
        match v {
            Value::String(s) => {
                data.insert(k.clone(), s.clone());
            }
            Value::Number(n) => {
                data.insert(k.clone(), n.to_string());
            }
            Value::Bool(b) => {
                data.insert(k.clone(), bool_to_string(*b));
            }
            Value::Array(_) | Value::Object(_) => {
                join_data.insert(k.clone(), v.clone());
            }
            Value::Null => {}
        }
    }

    (data, join_data)
}

/// Execute `find` — paginated query with filters, search, and population.
pub(super) fn exec_find(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let limit = args.get("limit").and_then(|v| v.as_i64());
    let page = args.get("page").and_then(|v| v.as_i64());
    let after_cursor = args.get("after_cursor").and_then(|v| v.as_str());
    let before_cursor = args.get("before_cursor").and_then(|v| v.as_str());

    let pg_ctx = query::PaginationCtx::new(
        config.pagination.default_limit,
        config.pagination.max_limit,
        config.pagination.is_cursor(),
    );
    let pagination = pg_ctx
        .validate(limit, page, after_cursor, before_cursor)
        .map_err(|e| anyhow::anyhow!(e))?;

    let order_by = args
        .get("order_by")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let search = args
        .get("search")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let depth = depth.min(config.depth.max_depth);

    let mut fq = FindQuery::builder()
        .filters(parse_where_filters(args))
        .limit(pagination.limit);

    if let Some(ref ob) = order_by {
        fq = fq.order_by(ob.as_str());
    }
    if !pagination.has_cursor() {
        fq = fq.offset(pagination.offset);
    }
    if let Some(ref c) = pagination.after_cursor {
        fq = fq.after_cursor(c.clone());
    }
    if let Some(ref c) = pagination.before_cursor {
        fq = fq.before_cursor(c.clone());
    }
    if let Some(ref s) = search {
        fq = fq.search(s.as_str());
    }

    let fq = fq.build();
    let hooks = RunnerReadHooks {
        runner,
        conn: &conn,
    };
    let opts = ReadOptions {
        depth,
        registry: Some(registry.as_ref()),
        ..Default::default()
    };

    let result =
        find_documents(&conn, &hooks, slug, def, &fq, &opts).map_err(|e| e.into_anyhow())?;

    let cursor_has_more =
        if pagination.has_cursor() && (result.docs.len() as i64) < pagination.limit {
            Some(false)
        } else {
            None
        };

    let pr = if config.pagination.is_cursor() {
        query::PaginationResult::builder(&result.docs, result.total, pagination.limit).cursor(
            order_by.as_deref(),
            def.timestamps,
            pagination.before_cursor.is_some(),
            pagination.has_cursor(),
            cursor_has_more,
        )
    } else {
        query::PaginationResult::builder(&result.docs, result.total, pagination.limit)
            .page(pagination.page, pagination.offset)
    };

    let doc_values: Vec<Value> = result.docs.iter().map(doc_to_json).collect();
    let output = json!({
        "docs": doc_values,
        "pagination": serde_json::to_value(&pr)?,
    });
    Ok(serde_json::to_string_pretty(&output)?)
}

/// Execute `find_by_id` — single document lookup with population.
pub(super) fn exec_find_by_id(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let depth = args
        .get("depth")
        .and_then(|v| v.as_i64())
        .unwrap_or(config.depth.default_depth as i64) as i32;
    let depth = depth.min(config.depth.max_depth);

    let hooks = RunnerReadHooks {
        runner,
        conn: &conn,
    };
    let opts = ReadOptions {
        depth,
        registry: Some(registry.as_ref()),
        ..Default::default()
    };

    let doc =
        find_document_by_id(&conn, &hooks, slug, def, id, &opts).map_err(|e| e.into_anyhow())?;

    match doc {
        Some(d) => Ok(serde_json::to_string_pretty(&doc_to_json(&d))?),
        None => Ok(json!({ "error": "Document not found" }).to_string()),
    }
}

/// Execute `create` — create a new document.
pub(super) fn exec_create(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let password = if def.is_auth_collection() {
        args.get("password")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    if let Some(ref pw) = password {
        config.auth.password_policy.validate(pw)?;
    }

    let (data, join_data) = extract_data_from_args(args, &["password"]);

    let (doc, _ctx) = create_document(
        pool,
        runner,
        slug,
        def,
        WriteInput::builder(data, &join_data)
            .password(password.as_deref())
            .build(),
        None,
    )?;

    info!("MCP create {}: {}", slug, doc.id);

    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

/// Execute `update` — update an existing document.
pub(super) fn exec_update(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &CrapConfig,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    let password = if def.is_auth_collection() {
        args.get("password")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    } else {
        None
    };

    if let Some(ref pw) = password {
        config.auth.password_policy.validate(pw)?;
    }

    let (data, join_data) = extract_data_from_args(args, &["id", "password"]);

    let (doc, _ctx) = update_document(
        pool,
        runner,
        slug,
        id,
        def,
        WriteInput::builder(data, &join_data)
            .password(password.as_deref())
            .build(),
        None,
    )?;

    info!("MCP update {}: {}", slug, id);

    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

/// Execute `delete` — delete a document by ID.
pub(super) fn exec_delete(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    delete_document(pool, runner, slug, id, def, None, None, None)?;

    info!("MCP delete {}: {}", slug, id);

    Ok(json!({ "deleted": id }).to_string())
}

/// Execute `read_global` — read a global document.
pub(super) fn exec_read_global(
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let def = registry.globals.get(slug).context("Global not found")?;
    let conn = pool.get().context("DB connection")?;
    let hooks = RunnerReadHooks {
        runner,
        conn: &conn,
    };

    match get_global_document(&conn, &hooks, slug, def, None, None, None)
        .map_err(|e| e.into_anyhow())
    {
        Ok(d) => Ok(serde_json::to_string_pretty(&doc_to_json(&d))?),
        Err(e) => {
            // The global row may not exist yet (table missing or default row not inserted).
            let is_missing = e.chain().any(|cause| {
                let msg = cause.to_string();
                msg.contains("no such table") || msg.starts_with("Failed to get global")
            });

            if is_missing {
                Ok(json!({}).to_string())
            } else {
                Err(e).context(format!("Failed to read global '{}'", slug))
            }
        }
    }
}

/// Execute `update_global` — update a global document.
pub(super) fn exec_update_global(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let def = registry.globals.get(slug).context("Global not found")?;

    let (data, join_data) = extract_data_from_args(args, &[]);

    let (doc, _ctx) = update_global_document(
        pool,
        runner,
        slug,
        def,
        WriteInput::builder(data, &join_data).build(),
        None,
    )?;

    info!("MCP update global: {}", slug);

    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}
