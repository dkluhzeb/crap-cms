//! `bench` command entry point — load the project, build the hook
//! runner, and dispatch to the per-action handler.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};

use crate::{
    commands::{BenchAction, Project, open_project},
    hooks::HookRunner,
};

use super::{create, hooks, queries};

/// Run a bench subcommand.
///
/// # Errors
///
/// Returns an error if the config can't be loaded, the Lua VM fails to
/// initialize, the database pool can't be created, schema sync fails, or
/// the dispatched subcommand itself fails.
pub fn run(config_dir: &Path, action: BenchAction) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool: db_pool,
    } = open_project(&config_dir)?;

    let runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&cfg)
        .build()
        .context("Failed to create HookRunner")?;

    match action {
        BenchAction::Hooks {
            collection,
            iterations,
            hooks: hooks_filter,
            exclude,
            all,
            data,
        } => {
            let conn = db_pool.get().context("DB connection")?;
            hooks::run(&hooks::HookBenchParams {
                registry: &registry,
                runner: &runner,
                conn: &conn,
                collection: collection.as_deref(),
                iterations,
                hooks_filter: hooks_filter.as_deref(),
                exclude: exclude.as_deref(),
                run_all: all,
                user_data: data.as_deref(),
                locale: &cfg.locale,
            })
        }

        BenchAction::Queries {
            collection,
            explain,
            r#where,
        } => {
            let conn = db_pool.get().context("DB connection")?;
            queries::run(&queries::QueryBenchParams {
                registry: &registry,
                conn: &conn,
                collection: collection.as_deref(),
                explain,
                where_clause: r#where.as_deref(),
                locale: &cfg.locale,
            })
        }

        BenchAction::Create {
            collection,
            iterations,
            data,
            no_hooks,
            yes,
        } => create::run(&create::CreateBenchParams {
            registry: &registry,
            pool: &db_pool,
            runner: &runner,
            slug: &collection,
            iterations,
            user_data: data.as_deref(),
            no_hooks,
            yes,
            locale: &cfg.locale,
        }),
    }
}
