//! `user` command dispatcher.

use anyhow::{Context as _, Result};
use std::path::Path;

use crate::{
    commands::{UserAction, load_config_and_sync},
    config::CrapConfig,
};

use super::{
    create::{UserCreateParams, user_create},
    info::user_info,
    list::user_list,
    modify::{
        UserChangePasswordParams, UserDeleteParams, user_change_password, user_delete, user_lock,
        user_reset_totp, user_unlock, user_unverify, user_verify,
    },
};

/// Dispatch user management subcommands.
///
/// # Errors
///
/// Returns an error from the dispatched action (create / delete /
/// modify / list / lock / unlock) — collection not found, password
/// validation, DB constraint violations, etc.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: UserAction) -> Result<()> {
    let (pool, registry) = load_config_and_sync(config_dir)?;

    match action {
        UserAction::Create {
            collection,
            email,
            password,
            fields,
        } => {
            let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;
            user_create(UserCreateParams {
                pool: &pool,
                registry: &registry,
                collection: &collection,
                email,
                password,
                fields,
                password_policy: &cfg.auth.password_policy,
            })
        }
        UserAction::List { collection } => user_list(&pool, &registry, &collection),
        UserAction::Info {
            collection,
            email,
            id,
        } => user_info(&pool, &registry, &collection, email, id),
        UserAction::Delete {
            collection,
            email,
            id,
            confirm,
        } => {
            let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;
            user_delete(UserDeleteParams {
                pool: &pool,
                registry: &registry,
                locale: &cfg.locale,
                collection: &collection,
                email,
                id,
                confirm,
            })
        }
        UserAction::Lock {
            collection,
            email,
            id,
        } => user_lock(&pool, &registry, &collection, email, id),
        UserAction::Unlock {
            collection,
            email,
            id,
        } => user_unlock(&pool, &registry, &collection, email, id),
        UserAction::Verify {
            collection,
            email,
            id,
        } => user_verify(&pool, &registry, &collection, email, id),
        UserAction::Unverify {
            collection,
            email,
            id,
        } => user_unverify(&pool, &registry, &collection, email, id),
        UserAction::ResetTotp {
            collection,
            email,
            id,
            confirm,
        } => user_reset_totp(&pool, &registry, &collection, email, id, confirm),
        UserAction::ChangePassword {
            collection,
            email,
            id,
            password,
        } => {
            let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;

            user_change_password(UserChangePasswordParams {
                pool: &pool,
                registry: &registry,
                collection: &collection,
                email,
                id,
                password,
                password_policy: &cfg.auth.password_policy,
            })
        }
    }
}
