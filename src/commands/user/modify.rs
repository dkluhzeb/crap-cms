//! User modification commands — delete, lock, unlock, verify, unverify, change password.

use anyhow::{Context as _, Result, anyhow};
use dialoguer::{Confirm, Password};

use crate::{
    cli::{self, crap_theme},
    config::{LocaleConfig, PasswordPolicy},
    core::SharedRegistry,
    db::{DbPool, query},
    service::{self, ServiceContext},
};

use super::helpers::{get_user_email, require_verify_email, resolve_user};

/// Delete a user from an auth collection.
#[cfg(not(tarpaulin_include))]
pub fn user_delete(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
    confirm: bool,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;
    let user_email = get_user_email(&doc);

    if !confirm {
        let proceed = Confirm::with_theme(&crap_theme())
            .with_prompt(format!(
                "Delete user {} ({}) from '{}'?",
                doc.id, user_email, collection
            ))
            .default(false)
            .interact()
            .context("Failed to read confirmation")?;

        if !proceed {
            cli::info("Aborted.");

            return Ok(());
        }
    }

    let mut conn = pool.get().context("Failed to get database connection")?;
    let reg = registry
        .read()
        .map_err(|_| anyhow!("Failed to read registry"))?;
    let def = reg
        .get_collection(collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found in registry", collection))?;
    let lc = LocaleConfig::default();

    let tx = conn
        .transaction_immediate()
        .context("Failed to start transaction")?;

    query::ref_count::before_hard_delete(&tx, collection, &doc.id, &def.fields, &lc)
        .context("Failed to adjust ref counts")?;

    query::delete(&tx, collection, &doc.id).context("Failed to delete user")?;

    tx.commit().context("Failed to commit delete transaction")?;

    cli::success(&format!(
        "Deleted user {} ({}) from '{}'",
        doc.id, user_email, collection
    ));

    Ok(())
}

/// Lock a user account.
#[cfg(not(tarpaulin_include))]
pub fn user_lock(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::lock_user(&ctx, &doc.id)
        .map_err(|e| e.into_anyhow())
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
#[cfg(not(tarpaulin_include))]
pub fn user_unlock(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::unlock_user(&ctx, &doc.id)
        .map_err(|e| e.into_anyhow())
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
pub fn user_verify(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;
    require_verify_email(&def, collection)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::mark_verified(&ctx, &doc.id)
        .map_err(|e| e.into_anyhow())
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
pub fn user_unverify(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

    require_verify_email(&def, collection)?;

    let conn = pool.get().context("Failed to get database connection")?;

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    service::auth::mark_unverified(&ctx, &doc.id)
        .map_err(|e| e.into_anyhow())
        .context("Failed to unverify user")?;

    cli::success(&format!(
        "Unverified user {} ({}) in '{}'",
        doc.id,
        get_user_email(&doc),
        collection
    ));

    Ok(())
}

/// Change a user's password.
#[cfg(not(tarpaulin_include))]
pub fn user_change_password(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
    password: Option<String>,
    password_policy: &PasswordPolicy,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

    let password = match password {
        Some(p) => {
            cli::warning("Password provided via command line — it may be visible in shell history");
            p
        }
        None => Password::with_theme(&crap_theme())
            .with_prompt("New password")
            .with_confirmation("Confirm password", "Passwords do not match")
            .interact()
            .context("Failed to read password")?,
    };

    password_policy.validate(&password)?;

    let conn = pool.get().context("Failed to get database connection")?;

    query::update_password(&conn, collection, &doc.id, &password)
        .context("Failed to update password")?;

    cli::success(&format!(
        "Password changed for user {} ({}) in '{}'",
        doc.id,
        get_user_email(&doc),
        collection
    ));

    Ok(())
}
