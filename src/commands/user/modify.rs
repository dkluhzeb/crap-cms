//! User modification commands — delete, lock, unlock, verify, unverify,
//! reset TOTP, change password.

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use dialoguer::Confirm;

use crate::{
    cli::{self, crap_theme},
    commands::cli_infra,
    config::CrapConfig,
    core::{CollectionDefinition, Document, Registry},
    db::{DbPool, query},
    service::{self, AppInfra, ServiceContext, ServiceError, auth::AccountAction},
};

use super::helpers::{
    UserLookup, get_user_email, require_verify_email, resolve_new_password, resolve_user,
};

/// Args for [`user_delete`].
pub struct UserDeleteParams<'a> {
    pub pool: &'a DbPool,
    pub registry: &'a Arc<Registry>,
    pub config: &'a CrapConfig,
    pub config_dir: &'a Path,
    pub collection: &'a str,
    pub email: Option<String>,
    pub id: Option<String>,
    pub confirm: bool,
}

/// Ask the operator to confirm the delete; `false` when they decline.
fn confirm_delete(doc: &Document, email: &str, collection: &str) -> Result<bool> {
    Confirm::with_theme(&crap_theme())
        .with_prompt(format!(
            "Delete user {} ({email}) from '{collection}'?",
            doc.id
        ))
        .default(false)
        .interact()
        .context("Failed to read confirmation")
}

/// Delete through the service layer, like every other surface: a user other
/// documents reference is refused, a soft-delete collection moves the user to
/// the trash, delete hooks run, upload files are cleaned up and the cache is
/// cleared. With live updates over Redis, the delete reaches `serve`'s
/// subscribers through `infra`'s transports and tears down the user's open
/// streams there. Collection access rules don't apply to the operator's CLI.
fn delete_through_service(
    p: &UserDeleteParams<'_>,
    infra: &AppInfra,
    def: &CollectionDefinition,
    id: &str,
) -> Result<()> {
    let ctx = ServiceContext::collection(p.collection, def)
        .infra(infra)
        .override_access(true)
        .build();

    service::delete_document(&ctx, id, Some(&*infra.storage), Some(&p.config.locale))
        .map_err(ServiceError::into_anyhow)
        .context("Failed to delete user")?;

    Ok(())
}

/// Delete a user from an auth collection.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the prompt fails, the user
/// is still referenced by other documents, or the delete fails.
#[cfg(not(tarpaulin_include))]
pub fn user_delete(p: &UserDeleteParams<'_>) -> Result<()> {
    let (_, doc) = resolve_user(&UserLookup {
        pool: p.pool,
        registry: p.registry,
        collection: p.collection,
        email: p.email.clone(),
        id: p.id.clone(),
        locale: &p.config.locale,
    })?;
    let user_email = get_user_email(&doc);

    // Built — and a configured Redis reached — before the prompt, so a delete
    // that couldn't reach `serve`'s subscribers fails before it is confirmed.
    let infra = cli_infra(p.config_dir, p.registry, p.config, p.pool)?;

    if !p.confirm && !confirm_delete(&doc, user_email, p.collection)? {
        cli::info("Aborted.");

        return Ok(());
    }

    let def = p
        .registry
        .get_collection(p.collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found in registry", p.collection))?;

    delete_through_service(p, &infra, def, &doc.id)?;

    let outcome = if def.soft_delete {
        "Moved user to the trash"
    } else {
        "Deleted user"
    };
    cli::success(&format!(
        "{outcome} {} ({}) in '{}'",
        doc.id, user_email, p.collection
    ));

    Ok(())
}

/// The operator's wording for an account-state action: the verb for an error,
/// the past tense for the success line.
fn action_words(action: AccountAction) -> (&'static str, &'static str) {
    match action {
        AccountAction::Lock => ("lock", "Locked"),
        AccountAction::Unlock => ("unlock", "Unlocked"),
        AccountAction::Verify => ("verify", "Verified"),
        AccountAction::Unverify => ("unverify", "Unverified"),
    }
}

/// Lock, unlock, verify or unverify a user through the service op, on the
/// CLI's infrastructure (`cli_infra`): a lock or an unverify bumps the
/// session version and — with live updates over Redis — tears down the
/// user's open streams on `serve`, like the same action from the admin or
/// gRPC. Collection access rules don't apply to the operator's CLI.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, a verification action
/// targets a collection without `verify_email`, or the write fails.
#[cfg(not(tarpaulin_include))]
pub fn user_account_action(
    lookup: &UserLookup<'_>,
    infra: &AppInfra,
    action: AccountAction,
) -> Result<()> {
    let collection = lookup.collection;
    let (def, doc) = resolve_user(lookup)?;

    if action.is_verification_action() {
        require_verify_email(&def, collection)?;
    }

    let ctx = ServiceContext::collection(collection, &def)
        .infra(infra)
        .override_access(true)
        .build();

    let (verb, done) = action_words(action);

    service::auth::apply_account_action(&ctx, &doc.id, action)
        .map_err(ServiceError::into_anyhow)
        .with_context(|| format!("Failed to {verb} user"))?;

    cli::success(&format!(
        "{done} user {} ({}) in '{collection}'",
        doc.id,
        get_user_email(&doc),
    ));

    Ok(())
}

/// Reset a user's TOTP enrollment: clears the sealed secret, the confirmed
/// flag, and the replay guard — the next MFA challenge re-provisions from
/// scratch (trust-on-first-login re-opens, so confirm interactively).
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the collection doesn't
/// use `mfa = "totp"`, the prompt fails, or the DB update fails.
#[cfg(not(tarpaulin_include))]
pub fn user_reset_totp(lookup: &UserLookup<'_>, confirm: bool) -> Result<()> {
    let (pool, collection) = (lookup.pool, lookup.collection);
    let (def, doc) = resolve_user(lookup)?;

    let uses_totp = def
        .auth
        .as_ref()
        .is_some_and(|a| a.mfa() == crate::core::collection::MfaMode::Totp);
    if !uses_totp {
        return Err(anyhow!(
            "Collection '{collection}' does not use mfa = \"totp\""
        ));
    }

    let user_email = get_user_email(&doc);

    if !confirm {
        let proceed = Confirm::with_theme(&crap_theme())
            .with_prompt(format!(
                "Reset TOTP enrollment for {} ({})? They re-enroll on their next login \
                 — anyone holding their password could enroll during that window.",
                doc.id, user_email
            ))
            .default(false)
            .interact()
            .context("Failed to read confirmation")?;

        if !proceed {
            cli::info("Aborted.");

            return Ok(());
        }
    }

    let conn = pool.get().context("Failed to get database connection")?;

    query::reset_totp(&conn, collection, &doc.id).context("Failed to reset TOTP enrollment")?;

    cli::success(&format!(
        "Reset TOTP enrollment for user {} ({}) in '{}' — they re-enroll on next login",
        doc.id, user_email, collection
    ));

    Ok(())
}

/// The new password for [`user_change_password`]: given inline, read from
/// stdin, or prompted for when neither is set.
pub struct UserChangePasswordParams {
    pub password: Option<String>,
    pub password_stdin: bool,
}

impl UserChangePasswordParams {
    /// A new password given inline (`password`) or read from stdin.
    #[must_use]
    pub fn new(password: Option<String>, password_stdin: bool) -> Self {
        Self {
            password,
            password_stdin,
        }
    }
}

/// Change a user's password through the service op, on the CLI's
/// infrastructure (`cli_infra`): the configured password policy applies,
/// every session opened with the old password ends, and — with live updates
/// over Redis — the user's open streams on `serve` are torn down, like the
/// same change from the admin or gRPC. Collection access rules don't apply to
/// the operator's CLI.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the password prompt fails,
/// the password fails policy validation, or the DB update fails.
#[cfg(not(tarpaulin_include))]
pub fn user_change_password(
    lookup: &UserLookup<'_>,
    infra: &AppInfra,
    p: UserChangePasswordParams,
) -> Result<()> {
    let collection = lookup.collection;
    let (def, doc) = resolve_user(lookup)?;

    let password = resolve_new_password(p.password, p.password_stdin, "New password")?;

    let ctx = ServiceContext::collection(collection, &def)
        .infra(infra)
        .override_access(true)
        .build();

    service::auth::set_password(&ctx, &doc.id, &password)
        .map_err(ServiceError::into_anyhow)
        .context("Failed to change password")?;

    cli::success(&format!(
        "Password changed for user {} ({}) in '{collection}'",
        doc.id,
        get_user_email(&doc),
    ));

    Ok(())
}
