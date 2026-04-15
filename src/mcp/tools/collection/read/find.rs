//! Execute `find` — paginated query with filters, search, and population.

use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json, to_string_pretty, to_value};

use crate::{
    config::CrapConfig,
    core::Registry,
    db::{DbPool, FindQuery, LocaleContext, query},
    hooks::HookRunner,
    mcp::tools::collection::helpers::{doc_to_json, parse_where_filters},
    service::{FindDocumentsInput, RunnerReadHooks, ServiceContext, find_documents},
};

/// Execute `find` — paginated query with filters, search, and population.
pub(in crate::mcp::tools) fn exec_find(
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
        .map_err(|e| anyhow!(e))?;

    let order_by = args
        .get("order_by")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let search = args
        .get("search")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let locale = args.get("locale").and_then(|v| v.as_str());
    let locale_ctx = LocaleContext::from_locale_string(locale, &config.locale)?;

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

    let is_trash = args.get("trash").and_then(|v| v.as_bool()).unwrap_or(false) && def.soft_delete;
    let include_drafts = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    // Default sort for trash listings is a presentation concern — keep here.
    if is_trash && order_by.is_none() {
        fq = fq.order_by("-_deleted_at");
    }

    let fq = fq.build();

    let hooks = RunnerReadHooks::new(runner, &conn);
    let ctx = ServiceContext::collection(slug, def)
        .pool(pool)
        .conn(&conn)
        .read_hooks(&hooks)
        .override_access(true)
        .build();

    let input = FindDocumentsInput::builder(&fq)
        .depth(depth)
        .locale_ctx(locale_ctx.as_ref())
        .registry(Some(registry.as_ref()))
        .cursor_enabled(config.pagination.is_cursor())
        .trash(is_trash)
        .include_drafts(include_drafts)
        .build();

    let result = find_documents(&ctx, &input).map_err(|e| e.into_anyhow())?;

    let doc_values: Vec<Value> = result.docs.iter().map(doc_to_json).collect();
    let output = json!({
        "docs": doc_values,
        "pagination": to_value(&result.pagination)?,
    });
    Ok(to_string_pretty(&output)?)
}
