//! `bench queries` — time find queries on each collection.

use std::time::Instant;

use anyhow::{Result, anyhow};

use crate::{
    api::handlers::convert::parse_where_json,
    cli::{self, Table},
    core::{CollectionDefinition, SharedRegistry},
    db::{DbConnection, DbValue, FindQuery, query},
};

use super::helpers::format_duration;

/// Run query benchmarks on all (or filtered) collections.
pub fn run(
    registry: &SharedRegistry,
    conn: &dyn DbConnection,
    collection: Option<&str>,
    explain: bool,
    where_clause: Option<&str>,
) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {e}"))?;

    let filters = match where_clause {
        Some(json_str) => {
            let parsed = parse_where_json(json_str).map_err(|e| anyhow!("Invalid --where: {e}"))?;
            cli::info(&format!("Filter: {json_str}"));
            Some(parsed)
        }
        None => None,
    };

    cli::header("Query Benchmarks");
    println!();

    let mut table = Table::new(vec!["Collection", "Rows", "Time", "Read hooks"]);
    let mut explain_output: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();

    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        if let Some(filter) = collection
            && slug.as_ref() as &str != filter
        {
            continue;
        }

        let def = &reg.collections[slug];

        let find_query = match &filters {
            Some(f) => FindQuery::builder().filters(f.clone()).limit(100).build(),
            None => FindQuery::builder().limit(100).build(),
        };

        let start = Instant::now();
        let result = query::find(conn, slug, def, &find_query, None);
        let elapsed = start.elapsed();

        let row_count = result.as_ref().map(|docs| docs.len()).unwrap_or(0);
        let read_hooks = collect_read_hooks(def);
        let hook_summary = if read_hooks.is_empty() {
            "-".to_string()
        } else {
            format!("{}", read_hooks.len())
        };

        table.row(vec![
            slug.as_ref(),
            &row_count.to_string(),
            &format_duration(elapsed),
            &hook_summary,
        ]);

        if explain {
            match run_explain(conn, slug, &find_query, def) {
                Ok(lines) if !lines.is_empty() => {
                    explain_output.push((slug.to_string(), lines, read_hooks));
                }
                Ok(_) => {
                    if !read_hooks.is_empty() {
                        explain_output.push((slug.to_string(), Vec::new(), read_hooks));
                    }
                }
                Err(e) => {
                    cli::warning(&format!("EXPLAIN failed for {slug}: {e}"));
                }
            }
        }
    }

    table.print();

    if !explain_output.is_empty() {
        println!();
        cli::header("Query Details");

        for (slug, plan_lines, hooks) in &explain_output {
            println!();
            cli::info(&format!("{slug}:"));

            for line in plan_lines {
                cli::dim(&format!("  plan: {line}"));
            }

            if hooks.is_empty() {
                cli::dim("  hooks: (none)");
            } else {
                for hook in hooks {
                    cli::dim(&format!("  hook: {hook}"));
                }
            }
        }
    }

    Ok(())
}

/// Run EXPLAIN QUERY PLAN with the same filters as the benchmark query.
fn run_explain(
    conn: &dyn DbConnection,
    slug: &str,
    find_query: &FindQuery,
    def: &crate::core::CollectionDefinition,
) -> Result<Vec<String>> {
    if conn.kind() != "sqlite" {
        return Ok(vec!["(EXPLAIN only available for SQLite)".to_string()]);
    }

    let (sql, params) = build_explain_sql(conn, slug, find_query, def)?;
    let rows = conn.query_all(&sql, &params)?;

    let mut lines = Vec::new();

    for row in &rows {
        if let Ok(detail) = row.get_string("detail") {
            lines.push(detail);
        }
    }

    Ok(lines)
}

/// Collect read-path hooks for a collection (access.read, before_read, after_read).
fn collect_read_hooks(def: &CollectionDefinition) -> Vec<String> {
    let mut hooks = Vec::new();

    if let Some(ref f) = def.access.read {
        hooks.push(format!("access.read: {f}"));
    }

    for f in &def.hooks.before_read {
        hooks.push(format!("before_read: {f}"));
    }

    for f in &def.hooks.after_read {
        hooks.push(format!("after_read: {f}"));
    }

    hooks
}

/// Build the EXPLAIN QUERY PLAN SQL using the same WHERE clause as the find query.
fn build_explain_sql(
    conn: &dyn DbConnection,
    slug: &str,
    find_query: &FindQuery,
    def: &crate::core::CollectionDefinition,
) -> Result<(String, Vec<DbValue>)> {
    use crate::db::query::filter::{build_where_clause, resolve_filters};

    let resolved = resolve_filters(&find_query.filters, def, None)?;
    let mut params: Vec<DbValue> = Vec::new();

    let where_clause = build_where_clause(conn, &resolved, slug, &def.fields, None, &mut params)?;

    let mut sql = format!("EXPLAIN QUERY PLAN SELECT * FROM \"{slug}\"");

    if !where_clause.is_empty() {
        sql.push_str(&where_clause);
    }

    if def.soft_delete {
        if where_clause.is_empty() {
            sql.push_str(" WHERE _deleted_at IS NULL");
        } else {
            sql.push_str(" AND _deleted_at IS NULL");
        }
    }

    Ok((sql, params))
}
