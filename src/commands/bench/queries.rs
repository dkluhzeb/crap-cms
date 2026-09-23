//! `bench queries` — time find queries on each collection.

use std::time::Instant;

use anyhow::{Result, anyhow};

use crate::{
    api::handlers::proto::parse_where_json,
    cli::{self, Table},
    commands::cli_find,
    config::LocaleConfig,
    core::{CollectionDefinition, Document, Registry},
    db::{
        DbConnection, DbValue, FilterClause, FindQuery, LocaleContext,
        query::filter::build_where_clause,
    },
};

use super::helpers::format_duration;

/// Parameters for the query benchmark.
pub(super) struct QueryBenchParams<'a> {
    pub registry: &'a Registry,
    pub conn: &'a dyn DbConnection,
    pub collection: Option<&'a str>,
    pub explain: bool,
    pub where_clause: Option<&'a str>,
    pub locale: &'a LocaleConfig,
}

/// One collection's query plan and read hooks, for the details section.
type ExplainEntry = (String, Vec<String>, Vec<String>);

/// What the rows column shows: the row count, or `ERR` when the query failed.
/// A failed query must never read as an empty collection.
fn rows_cell(slug: &str, result: &Result<Vec<Document>>) -> String {
    match result {
        Ok(docs) => docs.len().to_string(),
        Err(e) => {
            cli::warning(&format!("Query failed for {slug}: {e:#}"));

            "ERR".to_string()
        }
    }
}

/// Parse the `--where` JSON into filters (none without it), announcing it
/// when present.
fn parse_filters(where_clause: Option<&str>) -> Result<Vec<FilterClause>> {
    let Some(json_str) = where_clause else {
        return Ok(Vec::new());
    };

    let parsed = parse_where_json(json_str).map_err(|e| anyhow!("Invalid --where: {e}"))?;
    cli::info(&format!("Filter: {json_str}"));

    Ok(parsed)
}

/// Summarize the read hooks count for the table.
fn hook_summary(read_hooks: &[String]) -> String {
    if read_hooks.is_empty() {
        return "-".to_string();
    }

    read_hooks.len().to_string()
}

/// Run query benchmarks on all (or filtered) collections.
pub fn run(params: &QueryBenchParams) -> Result<()> {
    let filters = parse_filters(params.where_clause)?;
    let locale_ctx = LocaleContext::default_for(params.locale);

    cli::header("Query Benchmarks");
    println!();

    let mut table = Table::new(vec!["Collection", "Rows", "Time", "Read hooks"]);
    let mut explain_output: Vec<ExplainEntry> = Vec::new();

    let mut slugs: Vec<_> = params.registry.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        if params
            .collection
            .is_some_and(|filter| slug.as_ref() as &str != filter)
        {
            continue;
        }

        let def = &params.registry.collections[slug];

        let find_query = FindQuery::builder()
            .filters(filters.clone())
            .limit(Some(100))
            .build();

        let start = Instant::now();
        let result = cli_find(params.conn, def, &find_query, params.locale);
        let elapsed = start.elapsed();

        let read_hooks = collect_read_hooks(def);

        table.row(vec![
            slug.as_ref(),
            &rows_cell(slug, &result),
            &format_duration(elapsed),
            &hook_summary(&read_hooks),
        ]);

        if params.explain {
            let target = ExplainTarget {
                slug,
                find_query: &find_query,
                def,
                locale_ctx: locale_ctx.as_ref(),
            };

            collect_explain(params.conn, &target, read_hooks, &mut explain_output);
        }
    }

    table.print();
    print_explain_output(&explain_output);

    Ok(())
}

/// The query an EXPLAIN runs for: the benchmarked collection and its query,
/// under the same locale the benchmark read with.
struct ExplainTarget<'a> {
    slug: &'a str,
    find_query: &'a FindQuery,
    def: &'a CollectionDefinition,
    locale_ctx: Option<&'a LocaleContext>,
}

/// Run the EXPLAIN for one collection and queue its details for printing.
fn collect_explain(
    conn: &dyn DbConnection,
    target: &ExplainTarget<'_>,
    read_hooks: Vec<String>,
    out: &mut Vec<ExplainEntry>,
) {
    let slug = target.slug;

    match run_explain(conn, target) {
        Ok(lines) if !lines.is_empty() => out.push((slug.to_string(), lines, read_hooks)),
        Ok(_) if !read_hooks.is_empty() => out.push((slug.to_string(), Vec::new(), read_hooks)),
        Ok(_) => {}
        Err(e) => cli::warning(&format!("EXPLAIN failed for {slug}: {e}")),
    }
}

/// Print the "Query Details" section, if any collection has details.
fn print_explain_output(explain_output: &[ExplainEntry]) {
    if explain_output.is_empty() {
        return;
    }

    println!();
    cli::header("Query Details");

    for (slug, plan_lines, hooks) in explain_output {
        println!();
        cli::info(&format!("{slug}:"));

        for line in plan_lines {
            cli::dim(&format!("  plan: {line}"));
        }

        if hooks.is_empty() {
            cli::dim("  hooks: (none)");
        }

        for hook in hooks {
            cli::dim(&format!("  hook: {hook}"));
        }
    }
}

/// Run EXPLAIN QUERY PLAN with the same filters as the benchmark query.
fn run_explain(conn: &dyn DbConnection, target: &ExplainTarget<'_>) -> Result<Vec<String>> {
    if !conn.is_sqlite() {
        return Ok(vec!["(EXPLAIN only available for SQLite)".to_string()]);
    }

    let (sql, params) = build_explain_sql(conn, target)?;
    let rows = conn.query_all(&sql, &params)?;

    let mut lines = Vec::new();

    for row in &rows {
        if let Ok(detail) = row.get_string("detail") {
            lines.push(detail);
        }
    }

    Ok(lines)
}

/// Collect read-path hooks for a collection (access.read, `before_read`, `after_read`).
fn collect_read_hooks(def: &CollectionDefinition) -> Vec<String> {
    let mut hooks = Vec::new();

    if let Some(ref f) = def.access.read {
        hooks.push(format!("access.read: {}", f.reference()));
    }

    for f in &def.hooks.before_read {
        hooks.push(format!("before_read: {}", f.reference()));
    }

    for f in &def.hooks.after_read {
        hooks.push(format!("after_read: {}", f.reference()));
    }

    hooks
}

/// Build the EXPLAIN QUERY PLAN SQL using the same WHERE clause as the find query.
fn build_explain_sql(
    conn: &dyn DbConnection,
    target: &ExplainTarget<'_>,
) -> Result<(String, Vec<DbValue>)> {
    let ExplainTarget {
        slug,
        find_query,
        def,
        locale_ctx,
    } = *target;
    let mut params: Vec<DbValue> = Vec::new();

    let where_clause = build_where_clause(
        conn,
        &find_query.filters,
        slug,
        &def.fields,
        locale_ctx,
        &mut params,
    )?;

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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::db::InMemoryConn;

    #[test]
    fn collect_read_hooks_gathers_access_then_before_then_after() {
        let mut def = CollectionDefinition::new("posts");
        def.access.read = Some("access.can_read".into());
        def.hooks.before_read = vec!["hooks.br1".into(), "hooks.br2".into()];
        def.hooks.after_read = vec!["hooks.ar1".into()];
        assert_eq!(
            collect_read_hooks(&def),
            vec![
                "access.read: access.can_read".to_string(),
                "before_read: hooks.br1".to_string(),
                "before_read: hooks.br2".to_string(),
                "after_read: hooks.ar1".to_string(),
            ]
        );
    }

    /// A failed query must show as an error, never as an empty collection.
    #[test]
    fn a_failed_query_shows_err_not_zero_rows() {
        let failed: Result<Vec<Document>> = Err(anyhow!("no such column: name"));
        assert_eq!(rows_cell("users", &failed), "ERR");

        let empty: Result<Vec<Document>> = Ok(Vec::new());
        assert_eq!(rows_cell("users", &empty), "0");
    }

    #[test]
    fn collect_read_hooks_is_empty_with_no_hooks() {
        assert!(collect_read_hooks(&CollectionDefinition::new("posts")).is_empty());
    }

    fn explain_sql(def: &CollectionDefinition, fq: &FindQuery) -> (String, Vec<DbValue>) {
        let target = ExplainTarget {
            slug: "posts",
            find_query: fq,
            def,
            locale_ctx: None,
        };

        build_explain_sql(&InMemoryConn::open(), &target).unwrap()
    }

    #[test]
    fn build_explain_sql_base_and_soft_delete_variants() {
        let fq = FindQuery::default();
        let mut def = CollectionDefinition::new("posts");

        let (sql, params) = explain_sql(&def, &fq);
        assert_eq!(sql, "EXPLAIN QUERY PLAN SELECT * FROM \"posts\"");
        assert!(params.is_empty());

        def.soft_delete = true;
        let (sql2, _) = explain_sql(&def, &fq);
        assert_eq!(
            sql2,
            "EXPLAIN QUERY PLAN SELECT * FROM \"posts\" WHERE _deleted_at IS NULL"
        );
    }
}
