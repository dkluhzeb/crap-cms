//! `bench create` — time a full document create cycle (rolled back).

use std::{collections::HashMap, time::Instant};

use anyhow::{Result, anyhow};
use dialoguer::Confirm;
use serde_json::Value;

use crate::{
    cli::{self, crap_theme},
    core::SharedRegistry,
    db::DbPool,
    hooks::HookRunner,
    service::{RunnerWriteHooks, ServiceContext, WriteInput, create_document_core},
};

use super::helpers::{self, format_duration, timing_stats};

/// Parameters for the create benchmark.
pub struct CreateBenchParams<'a> {
    pub registry: &'a SharedRegistry,
    pub pool: &'a DbPool,
    pub runner: &'a HookRunner,
    pub slug: &'a str,
    pub iterations: usize,
    pub user_data: Option<&'a str>,
    pub no_hooks: bool,
    pub yes: bool,
}

/// Run create benchmarks for a collection.
pub fn run(params: &CreateBenchParams) -> Result<()> {
    let reg = params
        .registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {e}"))?;

    let slug = params.slug;

    let def = reg
        .get_collection(slug)
        .ok_or_else(|| anyhow!("Collection '{slug}' not found"))?;

    cli::header(&format!("Create Benchmark: {slug}"));

    if !params.no_hooks && !params.yes {
        cli::warning(
            "Hooks will run during the benchmark. External side effects \
             (webhooks, API calls) CANNOT be rolled back.",
        );

        let confirmed = Confirm::with_theme(&crap_theme())
            .with_prompt("Continue?")
            .default(false)
            .interact()?;

        if !confirmed {
            cli::info("Aborted. Use --no-hooks to skip hooks, or -y to skip this prompt.");
            return Ok(());
        }
    }

    // Resolve data
    let conn = params.pool.get()?;
    let (data, source) = helpers::resolve_bench_data(&conn, slug, def, params.user_data)?;
    drop(conn);

    let join_data: HashMap<String, Value> = HashMap::new();

    cli::kv("Iterations", &params.iterations.to_string());
    cli::kv("Data source", source.label());

    if params.no_hooks {
        cli::kv("Hooks", "disabled");
    }

    println!();

    let mut durations = Vec::with_capacity(params.iterations);
    let mut errors = 0;

    for i in 0..params.iterations {
        // Re-randomize unique fields per iteration to avoid uniqueness violations
        let mut iter_data = data.clone();
        helpers::randomize_unique_fields(&mut iter_data, &def.fields);
        let data_str = helpers::to_string_map(&iter_data);

        let mut conn = params.pool.get()?;
        let tx = conn.transaction()?;

        let mut wh = RunnerWriteHooks::new(params.runner)
            .with_conn(&tx)
            .with_override_access();

        if params.no_hooks {
            wh.hooks_enabled = false;
        }

        let ctx = ServiceContext::collection(slug, def)
            .conn(&tx)
            .write_hooks(&wh)
            .override_access(true)
            .build();

        let input = WriteInput::builder(data_str, &join_data).build();

        let start = Instant::now();
        let result = create_document_core(&ctx, input);
        let elapsed = start.elapsed();

        durations.push(elapsed);

        if let Err(e) = result {
            errors += 1;
            if i == 0 {
                cli::warning(&format!("Iteration 1 error: {e}"));
            }
        }

        // Drop ctx before tx to release borrow on tx
        drop(ctx);
        // Transaction dropped without commit — auto-rollback
    }

    let (min, avg, max) = timing_stats(&durations);

    println!();
    cli::kv("Min", &format_duration(min));
    cli::kv("Avg", &format_duration(avg));
    cli::kv("Max", &format_duration(max));

    if errors > 0 {
        cli::kv_status("Errors", &format!("{errors}/{}", params.iterations), false);
    }

    println!();
    cli::hint("Documents were not persisted (transaction rolled back).");

    Ok(())
}
