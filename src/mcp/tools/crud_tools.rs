//! CRUD tool implementations: find, find_by_id, create, update, delete, and global ops.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use serde_json::{json, Value};

use crate::core::document::Document;
use crate::core::Registry;
use crate::db::query::{self, FindQuery};
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

/// Parse JSON `where` object into filter clauses.
/// Supports `{ field: "value" }` (equals) and `{ field: { op: value } }` (operator-based).
pub(super) fn parse_where_filters(args: &Value) -> Vec<query::FilterClause> {
    let where_val = match args.get("where") {
        Some(v) if v.is_object() => v,
        _ => return Vec::new(),
    };
    let map = match where_val.as_object() {
        Some(m) => m,
        None => return Vec::new(),
    };

    let mut clauses = Vec::new();
    for (field, value) in map {
        match value {
            Value::String(s) => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.clone(),
                    op: query::FilterOp::Equals(s.clone()),
                }));
            }
            Value::Number(n) => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.clone(),
                    op: query::FilterOp::Equals(n.to_string()),
                }));
            }
            Value::Bool(b) => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.clone(),
                    op: query::FilterOp::Equals(if *b { "1" } else { "0" }.to_string()),
                }));
            }
            Value::Object(ops) => {
                for (op_name, op_value) in ops {
                    // Handle array-valued operators (in, not_in)
                    if matches!(op_name.as_str(), "in" | "not_in") {
                        if let Some(arr) = op_value.as_array() {
                            let vals: Vec<String> = arr
                                .iter()
                                .filter_map(|v| match v {
                                    Value::String(s) => Some(s.clone()),
                                    Value::Number(n) => Some(n.to_string()),
                                    _ => None,
                                })
                                .collect();
                            let op = match op_name.as_str() {
                                "in" => query::FilterOp::In(vals),
                                "not_in" => query::FilterOp::NotIn(vals),
                                _ => unreachable!(),
                            };
                            clauses.push(query::FilterClause::Single(query::Filter {
                                field: field.clone(),
                                op,
                            }));
                        }
                        continue;
                    }
                    // Handle value-less operators (exists, not_exists)
                    if matches!(op_name.as_str(), "exists" | "not_exists") {
                        let op = match op_name.as_str() {
                            "exists" => query::FilterOp::Exists,
                            "not_exists" => query::FilterOp::NotExists,
                            _ => unreachable!(),
                        };
                        clauses.push(query::FilterClause::Single(query::Filter {
                            field: field.clone(),
                            op,
                        }));
                        continue;
                    }
                    // Scalar-valued operators
                    let val_str = match op_value {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
                        _ => continue,
                    };
                    let op = match op_name.as_str() {
                        "equals" => query::FilterOp::Equals(val_str),
                        "not_equals" => query::FilterOp::NotEquals(val_str),
                        "contains" => query::FilterOp::Contains(val_str),
                        "greater_than" => query::FilterOp::GreaterThan(val_str),
                        "greater_than_equal" => query::FilterOp::GreaterThanOrEqual(val_str),
                        "less_than" => query::FilterOp::LessThan(val_str),
                        "less_than_equal" => query::FilterOp::LessThanOrEqual(val_str),
                        "like" => query::FilterOp::Like(val_str),
                        _ => continue,
                    };
                    clauses.push(query::FilterClause::Single(query::Filter {
                        field: field.clone(),
                        op,
                    }));
                }
            }
            _ => {}
        }
    }
    clauses
}

pub(super) fn doc_to_json(doc: &Document) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".to_string(), Value::String(doc.id.clone()));
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

pub(super) fn exec_find(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    _runner: &HookRunner,
    config: &crate::config::CrapConfig,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let limit = args.get("limit").and_then(|v| v.as_i64());
    let limit = query::apply_pagination_limits(
        limit,
        config.pagination.default_limit,
        config.pagination.max_limit,
    );
    let offset = args.get("offset").and_then(|v| v.as_i64());
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
    let filters = parse_where_filters(args);

    let mut fq = FindQuery::new();
    fq.filters = filters;
    fq.order_by = order_by;
    fq.limit = Some(limit);
    fq.offset = offset;
    fq.search = search;
    let mut docs = query::find(&conn, slug, def, &fq, None)?;
    let total = query::count(&conn, slug, def, &fq.filters, None)?;

    if depth > 0 {
        let pop_ctx = query::PopulateContext {
            conn: &conn,
            registry,
            collection_slug: slug,
            def,
        };
        let pop_opts = query::PopulateOpts {
            depth,
            select: None,
            locale_ctx: None,
        };
        query::populate_relationships_batch(&pop_ctx, &mut docs, &pop_opts)?;
    }

    let result = json!({
        "docs": docs.iter().map(doc_to_json).collect::<Vec<_>>(),
        "totalDocs": total,
        "limit": limit,
        "offset": offset.unwrap_or(0),
    });
    Ok(serde_json::to_string_pretty(&result)?)
}

pub(super) fn exec_find_by_id(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    config: &crate::config::CrapConfig,
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

    let mut doc = match query::find_by_id(&conn, slug, def, id, None)? {
        Some(d) => d,
        None => return Ok(json!({ "error": "Document not found" }).to_string()),
    };

    if depth > 0 {
        let mut visited = std::collections::HashSet::new();
        let pop_ctx = query::PopulateContext {
            conn: &conn,
            registry,
            collection_slug: slug,
            def,
        };
        let pop_opts = query::PopulateOpts {
            depth,
            select: None,
            locale_ctx: None,
        };
        query::populate_relationships(&pop_ctx, &mut doc, &mut visited, &pop_opts)?;
    }

    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

pub(super) fn exec_create(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &crate::config::CrapConfig,
) -> Result<String> {
    let def = registry
        .collections
        .get(slug)
        .context("Collection not found")?;

    // Extract password for auth collections
    let password = if def.is_auth_collection() {
        args.get("password")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    // Validate password against policy
    if let Some(ref pw) = password {
        config.auth.password_policy.validate(pw)?;
    }

    // Convert args to string map for create
    let mut data = HashMap::new();
    let mut join_data = HashMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if k == "password" {
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
                    data.insert(
                        k.clone(),
                        if *b { "1".to_string() } else { "0".to_string() },
                    );
                }
                Value::Array(_) | Value::Object(_) => {
                    join_data.insert(k.clone(), v.clone());
                }
                Value::Null => {}
            }
        }
    }

    let (doc, _ctx) = crate::service::create_document(
        pool,
        runner,
        slug,
        def,
        crate::service::WriteInput {
            data,
            join_data: &join_data,
            password: password.as_deref(),
            locale_ctx: None,
            locale: None,
            draft: false,
            ui_locale: None,
        },
        None,
    )?;

    tracing::info!("MCP create {}: {}", slug, doc.id);
    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

pub(super) fn exec_update(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
    config: &crate::config::CrapConfig,
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

    // Validate password against policy
    if let Some(ref pw) = password {
        config.auth.password_policy.validate(pw)?;
    }

    let mut data = HashMap::new();
    let mut join_data = HashMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if k == "id" || k == "password" {
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
                    data.insert(
                        k.clone(),
                        if *b { "1".to_string() } else { "0".to_string() },
                    );
                }
                Value::Array(_) | Value::Object(_) => {
                    join_data.insert(k.clone(), v.clone());
                }
                Value::Null => {}
            }
        }
    }

    let (doc, _ctx) = crate::service::update_document(
        pool,
        runner,
        slug,
        id,
        def,
        crate::service::WriteInput {
            data,
            join_data: &join_data,
            password: password.as_deref(),
            locale_ctx: None,
            locale: None,
            draft: false,
            ui_locale: None,
        },
        None,
    )?;

    tracing::info!("MCP update {}: {}", slug, id);
    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

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

    crate::service::delete_document(pool, runner, slug, id, def, None, None)?;

    tracing::info!("MCP delete {}: {}", slug, id);
    Ok(json!({ "deleted": id }).to_string())
}

pub(super) fn exec_read_global(
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
) -> Result<String> {
    let def = registry.globals.get(slug).context("Global not found")?;
    let conn = pool.get().context("DB connection")?;

    match query::get_global(&conn, slug, def, None) {
        Ok(d) => Ok(serde_json::to_string_pretty(&doc_to_json(&d))?),
        Err(e) => {
            // "not found" is expected for globals that haven't been written yet
            let err_msg = e.to_string();
            if err_msg.contains("not found") || err_msg.contains("no rows") {
                Ok(json!({}).to_string())
            } else {
                Err(e).context(format!("Failed to read global '{}'", slug))
            }
        }
    }
}

pub(super) fn exec_update_global(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let def = registry.globals.get(slug).context("Global not found")?;

    let mut data = HashMap::new();
    let mut join_data = HashMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            match v {
                Value::String(s) => {
                    data.insert(k.clone(), s.clone());
                }
                Value::Number(n) => {
                    data.insert(k.clone(), n.to_string());
                }
                Value::Bool(b) => {
                    data.insert(
                        k.clone(),
                        if *b { "1".to_string() } else { "0".to_string() },
                    );
                }
                Value::Array(_) | Value::Object(_) => {
                    join_data.insert(k.clone(), v.clone());
                }
                Value::Null => {}
            }
        }
    }

    let (doc, _ctx) = crate::service::update_global_document(
        pool,
        runner,
        slug,
        def,
        crate::service::WriteInput {
            data,
            join_data: &join_data,
            password: None,
            locale_ctx: None,
            locale: None,
            draft: false,
            ui_locale: None,
        },
        None,
    )?;

    tracing::info!("MCP update global: {}", slug);
    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}
