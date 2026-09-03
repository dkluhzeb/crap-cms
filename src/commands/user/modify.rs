//! User modification commands — delete, lock, unlock, verify, unverify, change password.

use anyhow::{Context as _, Result, anyhow};
use dialoguer::{Confirm, Password};

use crate::{
    cli::{self, crap_theme},
    config::{LocaleConfig, PasswordPolicy},
    core::Registry,
    db::{DbPool, query},
    service::{self, ServiceContext, ServiceError},
};

use super::helpers::{get_user_email, require_verify_email, resolve_user};

/// Args for [`user_delete`].
pub struct UserDeleteParams<'a> {
    pub pool: &'a DbPool,
    pub registry: &'a Registry,
    pub locale: &'a LocaleConfig,
    pub collection: &'a str,
    pub email: Option<String>,
    pub id: Option<String>,
    pub confirm: bool,
}

/// Delete a user from an auth collection.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the prompt fails, the DB
/// transaction fails, or any of the delete/ref-count steps fails.
#[cfg(not(tarpaulin_include))]
pub fn user_delete(p: UserDeleteParams<'_>) -> Result<()> {
    let (_, doc) = resolve_user(p.pool, p.registry, p.collection, p.email, p.id)?;
    let user_email = get_user_email(&doc);

    if !p.confirm {
        let proceed = Confirm::with_theme(&crap_theme())
            .with_prompt(format!(
                "Delete user {} ({}) from '{}'?",
                doc.id, user_email, p.collection
            ))
            .default(false)
            .interact()
            .context("Failed to read confirmation")?;

        if !proceed {
            cli::info("Aborted.");

            return Ok(());
        }
    }

    let mut conn = p.pool.get().context("Failed to get database connection")?;
    let reg = p.registry;
    let def = reg
        .get_collection(p.collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found in registry", p.collection))?;

    let tx = conn
        .transaction_immediate()
        .context("Failed to start transaction")?;

    query::ref_count::before_hard_delete(&tx, p.collection, &doc.id, &def.fields, p.locale)
        .context("Failed to adjust ref counts")?;

    query::delete(&tx, p.collection, &doc.id).context("Failed to delete user")?;

    tx.commit().context("Failed to commit delete transaction")?;

    cli::success(&format!(
        "Deleted user {} ({}) from '{}'",
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
pub fn user_lock(
    pool: &DbPool,
    registry: &Registry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

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
pub fn user_unlock(
    pool: &DbPool,
    registry: &Registry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

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
pub(super) fn user_verify(
    pool: &DbPool,
    registry: &Registry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;
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
pub(super) fn user_unverify(
    pool: &DbPool,
    registry: &Registry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

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
pub fn user_reset_totp(
    pool: &DbPool,
    registry: &Registry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
    confirm: bool,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

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
    pub password_policy: &'a PasswordPolicy,
}

/// Change a user's password.
///
/// # Errors
///
/// Returns an error if the user can't be resolved, the password prompt
/// fails, the password fails policy validation, or the DB update fails.
#[cfg(not(tarpaulin_include))]
pub fn user_change_password(p: UserChangePasswordParams<'_>) -> Result<()> {
    let (_, doc) = resolve_user(p.pool, p.registry, p.collection, p.email, p.id)?;

    let password = match p.password {
        Some(pw) => {
            cli::warning("Password provided via command line — it may be visible in shell history");
            pw
        }
        None => Password::with_theme(&crap_theme())
            .with_prompt("New password")
            .with_confirmation("Confirm password", "Passwords do not match")
            .interact()
            .context("Failed to read password")?,
    };

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
