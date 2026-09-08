//! `user` command dispatcher.

use anyhow::{Context as _, Result};
use std::path::Path;

use crate::{
    commands::{UserAction, load_config_and_sync},
    config::CrapConfig,
    core::Registry,
    db::DbPool,
};

use super::{
    create::{UserCreateParams, user_create},
    helpers::UserLookup,
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
    // One load for every subcommand: each needs at least the locale config to
    // read a (possibly localized) auth collection's rows.
    let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;

    match action {
        UserAction::Create {
            collection,
            email,
            password,
            fields,
        } => user_create(UserCreateParams {
            pool: &pool,
            registry: &registry,
            collection: &collection,
            email,
            password,
            fields,
            password_policy: &cfg.auth.password_policy,
            locale: &cfg.locale,
        }),
        UserAction::List { collection } => user_list(&pool, &registry, &collection),
        UserAction::Delete {
            collection,
            email,
            id,
            confirm,
        } => user_delete(UserDeleteParams {
            pool: &pool,
            registry: &registry,
            locale: &cfg.locale,
            collection: &collection,
            email,
            id,
            confirm,
        }),
        UserAction::ChangePassword {
            collection,
            email,
            id,
            password,
        } => user_change_password(UserChangePasswordParams {
            pool: &pool,
            registry: &registry,
            collection: &collection,
            email,
            id,
            password,
            password_policy: &cfg.auth.password_policy,
            locale: &cfg.locale,
        }),
        ref other => run_lookup_action(&pool, &registry, &cfg, other),
    }
}

/// The subcommands that only need to find one user and act on it. Split from
/// [`run`] so each half stays readable — they all build the same
/// [`UserLookup`].
#[cfg(not(tarpaulin_include))]
fn run_lookup_action(
    pool: &DbPool,
    registry: &Registry,
    cfg: &CrapConfig,
    action: &UserAction,
) -> Result<()> {
    // Pull the shared lookup arguments out once, then dispatch on the action.
    let (collection, email, id, confirm) = match action {
        UserAction::Info {
            collection,
            email,
            id,
        }
        | UserAction::Lock {
            collection,
            email,
            id,
        }
        | UserAction::Unlock {
            collection,
            email,
            id,
        }
        | UserAction::Verify {
            collection,
            email,
            id,
        }
        | UserAction::Unverify {
            collection,
            email,
            id,
        } => (collection.clone(), email.clone(), id.clone(), false),
        UserAction::ResetTotp {
            collection,
            email,
            id,
            confirm,
        } => (collection.clone(), email.clone(), id.clone(), *confirm),
        // Handled by `run`; listed so a new variant fails the build here.
        UserAction::Create { .. }
        | UserAction::List { .. }
        | UserAction::Delete { .. }
        | UserAction::ChangePassword { .. } => unreachable!("handled in run()"),
    };

    let lookup = UserLookup {
        pool,
        registry,
        collection: &collection,
        email,
        id,
        locale: &cfg.locale,
    };

    match action {
        UserAction::Info { .. } => user_info(&lookup),
        UserAction::Lock { .. } => user_lock(&lookup),
        UserAction::Unlock { .. } => user_unlock(&lookup),
        UserAction::Verify { .. } => user_verify(&lookup),
        UserAction::Unverify { .. } => user_unverify(&lookup),
        UserAction::ResetTotp { .. } => user_reset_totp(&lookup, confirm),
        _ => unreachable!("handled in run()"),
    }
}
