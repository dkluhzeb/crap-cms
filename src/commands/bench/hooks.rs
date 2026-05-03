//! `bench hooks` — time individual Lua hooks with interactive selection.

use std::time::Instant;

use anyhow::{Result, anyhow};
use dialoguer::MultiSelect;

use crate::{
    cli::{self, Table, crap_theme},
    core::{Registry, SharedRegistry, collection::Hooks},
    db::DbConnection,
    hooks::HookRunner,
};

use super::helpers::{self, DataSource, format_duration, timing_stats};

/// A hook to benchmark: collection slug, event name, function ref.
struct HookEntry {
    slug: String,
    event: &'static str,
    hook_ref: String,
}

impl HookEntry {
    fn label(&self) -> String {
        format!("{} / {}: {}", self.slug, self.event, self.hook_ref)
    }
}

/// Collect all hook entries from the registry.
fn collect_hooks(reg: &Registry, filter_collection: Option<&str>) -> Vec<HookEntry> {
    let mut entries = Vec::new();
    let mut slugs: Vec<_> = reg.collections.keys().collect();
    slugs.sort();

    for slug in slugs {
        if let Some(filter) = filter_collection
            && slug.as_ref() as &str != filter
        {
            continue;
        }

        let hooks = &reg.collections[slug].hooks;
        append_event_hooks(&mut entries, slug, hooks);
    }

    let mut global_slugs: Vec<_> = reg.globals.keys().collect();
    global_slugs.sort();

    for slug in global_slugs {
        if filter_collection.is_some() {
            continue; // --collection only filters collections
        }

        let hooks = &reg.globals[slug].hooks;
        append_event_hooks(&mut entries, slug, hooks);
    }

    entries
}

fn append_event_hooks(entries: &mut Vec<HookEntry>, slug: &str, hooks: &Hooks) {
    let events: &[(&str, &[String])] = &[
        ("before_validate", &hooks.before_validate),
        ("before_change", &hooks.before_change),
        ("after_change", &hooks.after_change),
        ("before_read", &hooks.before_read),
        ("after_read", &hooks.after_read),
        ("before_delete", &hooks.before_delete),
        ("after_delete", &hooks.after_delete),
        ("before_broadcast", &hooks.before_broadcast),
    ];

    for (event, refs) in events {
        for hook_ref in *refs {
            entries.push(HookEntry {
                slug: slug.to_string(),
                event,
                hook_ref: hook_ref.clone(),
            });
        }
    }
}

/// Select which hooks to run — interactive wizard, --hooks, --all, or --exclude.
fn select_hooks(
    all_hooks: &[HookEntry],
    cli_hooks: Option<&str>,
    cli_exclude: Option<&str>,
    run_all: bool,
) -> Result<Vec<usize>> {
    if run_all {
        cli::warning(
            "Running ALL hooks — some may have external side effects (API calls, webhooks).",
        );
        return Ok((0..all_hooks.len()).collect());
    }

    if let Some(include) = cli_hooks {
        let refs: Vec<&str> = include.split(',').map(|s| s.trim()).collect();
        let indices: Vec<usize> = all_hooks
            .iter()
            .enumerate()
            .filter(|(_, h)| refs.contains(&h.hook_ref.as_str()))
            .map(|(i, _)| i)
            .collect();

        if indices.is_empty() {
            anyhow::bail!("No matching hooks found for: {include}");
        }

        return Ok(indices);
    }

    if let Some(exclude) = cli_exclude {
        let refs: Vec<&str> = exclude.split(',').map(|s| s.trim()).collect();
        let indices: Vec<usize> = all_hooks
            .iter()
            .enumerate()
            .filter(|(_, h)| !refs.contains(&h.hook_ref.as_str()))
            .map(|(i, _)| i)
            .collect();

        return Ok(indices);
    }

    // Interactive wizard
    let labels: Vec<String> = all_hooks.iter().map(|h| h.label()).collect();

    let selections = MultiSelect::with_theme(&crap_theme())
        .with_prompt("Select hooks to benchmark (space to toggle, enter to confirm)")
        .items(&labels)
        .interact()?;

    if selections.is_empty() {
        anyhow::bail!("No hooks selected.");
    }

    Ok(selections)
}

/// Parameters for the hook benchmark.
pub struct HookBenchParams<'a> {
    pub registry: &'a SharedRegistry,
    pub runner: &'a HookRunner,
    pub conn: &'a dyn DbConnection,
    pub collection: Option<&'a str>,
    pub iterations: usize,
    pub hooks_filter: Option<&'a str>,
    pub exclude: Option<&'a str>,
    pub run_all: bool,
    pub user_data: Option<&'a str>,
}

/// Run the hook benchmark.
pub fn run(params: &HookBenchParams) -> Result<()> {
    let reg = params
        .registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {e}"))?;

    let all_hooks = collect_hooks(&reg, params.collection);

    if all_hooks.is_empty() {
        cli::dim("No hooks found.");
        return Ok(());
    }

    let selected = select_hooks(
        &all_hooks,
        params.hooks_filter,
        params.exclude,
        params.run_all,
    )?;

    cli::header("Hook Benchmarks");
    cli::kv("Iterations", &params.iterations.to_string());
    println!();

    let mut table = Table::new(vec![
        "Target", "Event", "Hook", "Min", "Avg", "Max", "Notes",
    ]);

    for &idx in &selected {
        let entry = &all_hooks[idx];

        // Resolve data for this hook's collection
        let def = reg.collections.get(entry.slug.as_str());
        let (data, source) = if let Some(def) = def {
            helpers::resolve_bench_data(params.conn, &entry.slug, def, params.user_data)?
        } else {
            // Global — use empty data
            (std::collections::HashMap::new(), DataSource::Synthetic)
        };

        // Build a Hooks struct with only this hook
        let single_hooks = build_single_hook(entry.event, &entry.hook_ref);

        // Run iterations
        let mut durations = Vec::with_capacity(params.iterations);
        let mut errored = false;

        for _ in 0..params.iterations {
            let ctx = crate::hooks::HookContext::builder(&entry.slug, "bench")
                .data(data.clone())
                .build();

            let event = match entry.event {
                "before_validate" => crate::hooks::HookEvent::BeforeValidate,
                "before_change" => crate::hooks::HookEvent::BeforeChange,
                "after_change" => crate::hooks::HookEvent::AfterChange,
                "before_read" => crate::hooks::HookEvent::BeforeRead,
                "after_read" => crate::hooks::HookEvent::AfterRead,
                "before_delete" => crate::hooks::HookEvent::BeforeDelete,
                "after_delete" => crate::hooks::HookEvent::AfterDelete,
                "before_broadcast" => crate::hooks::HookEvent::BeforeBroadcast,
                _ => continue,
            };

            let start = Instant::now();
            let result = params.runner.run_hooks(&single_hooks, event, ctx);
            durations.push(start.elapsed());

            if result.is_err() {
                errored = true;
            }
        }

        let (min, avg, max) = timing_stats(&durations);
        let mut notes = source.label().to_string();

        if errored {
            if !notes.is_empty() {
                notes.push_str(", ");
            }
            notes.push_str("errored");
        }

        table.row(vec![
            &entry.slug,
            entry.event,
            &entry.hook_ref,
            &format_duration(min),
            &format_duration(avg),
            &format_duration(max),
            &notes,
        ]);
    }

    table.print();
    cli::dim(&format!("  {} hook(s) benchmarked", selected.len()));

    Ok(())
}

/// Build a `Hooks` struct with only one event populated with a single ref.
fn build_single_hook(event: &str, hook_ref: &str) -> Hooks {
    let refs = vec![hook_ref.to_string()];
    let mut hooks = Hooks::new();

    match event {
        "before_validate" => hooks.before_validate = refs,
        "before_change" => hooks.before_change = refs,
        "after_change" => hooks.after_change = refs,
        "before_read" => hooks.before_read = refs,
        "after_read" => hooks.after_read = refs,
        "before_delete" => hooks.before_delete = refs,
        "after_delete" => hooks.after_delete = refs,
        "before_broadcast" => hooks.before_broadcast = refs,
        _ => {}
    }

    hooks
}
