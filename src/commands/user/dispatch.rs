//! `user` command dispatcher.

use anyhow::Result;
use std::path::Path;

use crate::{
    commands::{Project, UserAction, cli_infra, open_project},
    config::CrapConfig,
    core::Registry,
    db::DbPool,
    service::{AppInfra, auth::AccountAction},
};

use super::{
    create::{UserCreateParams, user_create},
    helpers::UserLookup,
    info::user_info,
    list::user_list,
    modify::{
        UserChangePasswordParams, UserDeleteParams, user_account_action, user_change_password,
        user_delete, user_reset_totp,
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
    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool,
    } = open_project(config_dir)?;

    match action {
        UserAction::Create {
            collection,
            email,
            password,
            password_stdin,
            fields,
        } => user_create(UserCreateParams {
            pool: &pool,
            registry: &registry,
            config: &cfg,
            config_dir,
            collection: &collection,
            email,
            password,
            password_stdin,
            fields,
        }),
        UserAction::List { collection } => user_list(&pool, &registry, &collection, &cfg.locale),
        UserAction::Delete {
            collection,
            email,
            id,
            confirm,
        } => user_delete(&UserDeleteParams {
            pool: &pool,
            registry: &registry,
            config: &cfg,
            config_dir,
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
            password_stdin,
        } => {
            // Built — and a configured Redis reached — before the password
            // prompt, so a change that couldn't reach `serve`'s live streams
            // fails first.
            let infra = cli_infra(config_dir, &registry, &cfg, &pool)?;

            let lookup = UserLookup {
                pool: &pool,
                registry: &registry,
                collection: &collection,
                email,
                id,
                locale: &cfg.locale,
            };

            user_change_password(
                &lookup,
                &infra,
                UserChangePasswordParams::new(password, password_stdin),
            )
        }
        ref other => {
            let Some(account) = account_action(other) else {
                return run_lookup_action(&pool, &registry, &cfg, other);
            };

            // The account-state writes run on the CLI's infrastructure, so a
            // lock or an unverify reaches `serve`'s live streams.
            let infra = cli_infra(config_dir, &registry, &cfg, &pool)?;

            run_account_action(&infra, &cfg, other, account)
        }
    }
}

/// The account-state action a subcommand performs, if it is one.
fn account_action(action: &UserAction) -> Option<AccountAction> {
    match action {
        UserAction::Lock { .. } => Some(AccountAction::Lock),
        UserAction::Unlock { .. } => Some(AccountAction::Unlock),
        UserAction::Verify { .. } => Some(AccountAction::Verify),
        UserAction::Unverify { .. } => Some(AccountAction::Unverify),
        _ => None,
    }
}

/// The lookup arguments every single-user subcommand carries — collection,
/// email, id — plus the `--confirm` flag where there is one.
fn lookup_args(action: &UserAction) -> (String, Option<String>, Option<String>, bool) {
    match action {
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
    }
}

/// Lock, unlock, verify or unverify one user on the CLI's infrastructure.
#[cfg(not(tarpaulin_include))]
fn run_account_action(
    infra: &AppInfra,
    cfg: &CrapConfig,
    action: &UserAction,
    account: AccountAction,
) -> Result<()> {
    let (collection, email, id, _) = lookup_args(action);

    let lookup = UserLookup {
        pool: &infra.pool,
        registry: &infra.registry,
        collection: &collection,
        email,
        id,
        locale: &cfg.locale,
    };

    user_account_action(&lookup, infra, account)
}

/// The read-or-reset subcommands that only need to find one user and act on
/// it — `info` and `reset-totp`.
#[cfg(not(tarpaulin_include))]
fn run_lookup_action(
    pool: &DbPool,
    registry: &Registry,
    cfg: &CrapConfig,
    action: &UserAction,
) -> Result<()> {
    let (collection, email, id, confirm) = lookup_args(action);

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
        UserAction::ResetTotp { .. } => user_reset_totp(&lookup, confirm),
        _ => unreachable!("handled in run()"),
    }
}
