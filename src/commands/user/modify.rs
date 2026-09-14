//! User modification commands — delete, lock, unlock, verify, unverify, change password.

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use dialoguer::Confirm;

use crate::{
    cli::{self, crap_theme},
    commands::helpers::create_live_transports,
    config::{CrapConfig, LocaleConfig, PasswordPolicy},
    core::{
        CollectionDefinition, Document, Registry,
        event::{SharedEventTransport, SharedInvalidationTransport},
        upload::create_storage_with_lease,
    },
    db::{DbPool, query},
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError},
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
/// the trash, delete hooks run and upload files are cleaned up. With live
/// updates over Redis, the delete reaches `serve`'s subscribers through
/// `transports` and tears down the user's open streams there. Collection access
/// rules don't apply to the operator's CLI.
fn delete_through_service(
    p: &UserDeleteParams<'_>,
    def: &Arc<CollectionDefinition>,
    id: &str,
    transports: (Option<SharedEventTransport>, SharedInvalidationTransport),
) -> Result<()> {
    let (event_transport, invalidation_transport) = transports;

    let hook_runner = HookRunner::builder()
        .config_dir(p.config_dir)
        .registry(Arc::clone(p.registry))
        .config(p.config)
        .invalidation_transport(invalidation_transport.clone())
        .build()?;
    let storage =
        create_storage_with_lease(p.config_dir, &p.config.upload, hook_runner.lua_lease())?;

    let ctx = ServiceContext::collection(p.collection, def)
        .pool(p.pool)
        .runner(&hook_runner)
        .override_access(true)
        .event_transport(event_transport)
        .invalidation_transport(Some(invalidation_transport))
        .build();

    service::delete_document(&ctx, id, Some(&*storage), Some(&p.config.locale))
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
    let transports = create_live_transports(p.config)?;

    if !p.confirm && !confirm_delete(&doc, user_email, p.collection)? {
        cli::info("Aborted.");

        return Ok(());
    }

    let def = p
        .registry
        .get_collection(p.collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found in registry", p.collection))?;

    delete_through_service(p, def, &doc.id, transports)?;

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

/// Lock a user account.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the connection fails, or
/// the lock operation fails.
#[cfg(not(tarpaulin_include))]
pub fn user_lock(lookup: &UserLookup<'_>) -> Result<()> {
    let (pool, collection) = (lookup.pool, lookup.collection);
    let (_, doc) = resolve_user(lookup)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::lock_user(&ctx, &doc.id)
        .map_err(ServiceError::into_anyhow)
        .context("Failed to lock user")?;

    cli::success(&format!(
        "Locked user {} ({}) in '{}'",
        doc.id,
        get_user_email(&doc),
        collection
    ));

    Ok(())
}

/// Unlock a user account.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the connection fails, or
/// the unlock operation fails.
#[cfg(not(tarpaulin_include))]
pub fn user_unlock(lookup: &UserLookup<'_>) -> Result<()> {
    let (pool, collection) = (lookup.pool, lookup.collection);
    let (_, doc) = resolve_user(lookup)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::unlock_user(&ctx, &doc.id)
        .map_err(ServiceError::into_anyhow)
        .context("Failed to unlock user")?;

    cli::success(&format!(
        "Unlocked user {} ({}) in '{}'",
        doc.id,
        get_user_email(&doc),
        collection
    ));

    Ok(())
}

/// Verify a user account (mark email as verified).
#[cfg(not(tarpaulin_include))]
pub(super) fn user_verify(lookup: &UserLookup<'_>) -> Result<()> {
    let (pool, collection) = (lookup.pool, lookup.collection);
    let (def, doc) = resolve_user(lookup)?;
    require_verify_email(&def, collection)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::mark_verified(&ctx, &doc.id)
        .map_err(ServiceError::into_anyhow)
        .context("Failed to verify user")?;

    cli::success(&format!(
        "Verified user {} ({}) in '{}'",
        doc.id,
        get_user_email(&doc),
        collection
    ));

    Ok(())
}

/// Unverify a user account (mark email as unverified).
#[cfg(not(tarpaulin_include))]
pub(super) fn user_unverify(lookup: &UserLookup<'_>) -> Result<()> {
    let (pool, collection) = (lookup.pool, lookup.collection);
    let (def, doc) = resolve_user(lookup)?;

    require_verify_email(&def, collection)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::mark_unverified(&ctx, &doc.id)
        .map_err(ServiceError::into_anyhow)
        .context("Failed to unverify user")?;

    cli::success(&format!(
        "Unverified user {} ({}) in '{}'",
        doc.id,
        get_user_email(&doc),
        collection
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

/// Args for [`user_change_password`].
pub struct UserChangePasswordParams<'a> {
    pub pool: &'a DbPool,
    pub registry: &'a Registry,
    pub collection: &'a str,
    pub email: Option<String>,
    pub id: Option<String>,
    pub password: Option<String>,
    pub password_stdin: bool,
    pub password_policy: &'a PasswordPolicy,
    pub locale: &'a LocaleConfig,
}

/// Change a user's password.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the password prompt
/// fails, the password fails policy validation, or the DB update fails.
#[cfg(not(tarpaulin_include))]
pub fn user_change_password(p: UserChangePasswordParams<'_>) -> Result<()> {
    let (_, doc) = resolve_user(&UserLookup {
        pool: p.pool,
        registry: p.registry,
        collection: p.collection,
        email: p.email,
        id: p.id,
        locale: p.locale,
    })?;

    let password = resolve_new_password(p.password, p.password_stdin, "New password")?;

    p.password_policy.validate(&password)?;

    let conn = p.pool.get().context("Failed to get database connection")?;

    query::update_password(&conn, p.collection, &doc.id, &password)
        .context("Failed to update password")?;

    cli::success(&format!(
        "Password changed for user {} ({}) in '{}'",
        doc.id,
        get_user_email(&doc),
        p.collection
    ));

    Ok(())
}
