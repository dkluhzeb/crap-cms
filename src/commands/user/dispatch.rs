//! `user` command dispatcher.

use anyhow::{Context as _, Result};
use std::path::Path;

use crate::{
    commands::{UserAction, load_config_and_sync},
    config::CrapConfig,
};

use super::{
    create::user_create,
    info::user_info,
    list::user_list,
    modify::{
        user_change_password, user_delete, user_lock, user_unlock, user_unverify, user_verify,
    },
};

/// Dispatch user management subcommands.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: UserAction) -> Result<()> {
    match action {
        UserAction::Create {
            collection,
            email,
            password,
            fields,
        } => {
            let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_create(
                &pool,
                &registry,
                &collection,
                email,
                password,
                fields,
                &cfg.auth.password_policy,
            )
        }
        UserAction::List { collection } => {
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_list(&pool, &registry, &collection)
        }
        UserAction::Info {
            collection,
            email,
            id,
        } => {
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_info(&pool, &registry, &collection, email, id)
        }
        UserAction::Delete {
            collection,
            email,
            id,
            confirm,
        } => {
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_delete(&pool, &registry, &collection, email, id, confirm)
        }
        UserAction::Lock {
            collection,
            email,
            id,
        } => {
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_lock(&pool, &registry, &collection, email, id)
        }
        UserAction::Unlock {
            collection,
            email,
            id,
        } => {
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_unlock(&pool, &registry, &collection, email, id)
        }
        UserAction::Verify {
            collection,
            email,
            id,
        } => {
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_verify(&pool, &registry, &collection, email, id)
        }
        UserAction::Unverify {
            collection,
            email,
            id,
        } => {
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_unverify(&pool, &registry, &collection, email, id)
        }
        UserAction::ChangePassword {
            collection,
            email,
            id,
            password,
        } => {
            let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;
            let (pool, registry) = load_config_and_sync(config_dir)?;

            user_change_password(
                &pool,
                &registry,
                &collection,
                email,
                id,
                password,
                &cfg.auth.password_policy,
            )
        }
    }
}
