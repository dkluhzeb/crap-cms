//! `bench` command — benchmark hooks, queries, and write cycles.

mod create;
mod helpers;
mod hooks;
mod queries;

use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{
    commands::BenchAction,
    config::CrapConfig,
    db::{migrate, pool},
    hooks as hook_init,
};

/// Run a bench subcommand.
pub fn run(config_dir: &Path, action: BenchAction) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hook_init::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let db_pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&db_pool, &registry, &cfg.locale)
        .context("Failed to sync database schema")?;

    let runner = crate::hooks::HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
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
            })
        }

        BenchAction::Queries {
            collection,
            explain,
            r#where,
        } => {
            let conn = db_pool.get().context("DB connection")?;
            queries::run(
                &registry,
                &conn,
                collection.as_deref(),
                explain,
                r#where.as_deref(),
            )
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
        }),
    }
}
