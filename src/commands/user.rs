//! `user` command — user management for auth collections.

use anyhow::{Context as _, Result, anyhow, bail};
use dialoguer::{Confirm, Input, Password, Select};
use serde_json::Value;
use std::{collections::HashMap, path::Path};

use super::{UserAction, load_config_and_sync};
use crate::{
    cli::{self, Table, crap_theme},
    config::{CrapConfig, PasswordPolicy},
    core::{CollectionDefinition, Document, SharedRegistry, field::FieldType},
    db::{DbPool, query},
};

/// Dispatch user management subcommands.
/// Untestable: dispatches to interactive CLI functions that require stdin/dialoguer.
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

/// Resolve a user by --email or --id. Returns (def, document).
/// Untestable: interactive fallback uses dialoguer::Select for user selection.
#[cfg(not(tarpaulin_include))]
fn resolve_user(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<(CollectionDefinition, Document)> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg
        .get_collection(collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found", collection))?;

    if !def.is_auth_collection() {
        bail!(
            "Collection '{}' is not an auth collection (auth must be enabled)",
            collection
        );
    }

    let def = def.clone();

    drop(reg);

    let conn = pool.get().context("Failed to get database connection")?;

    if let Some(email) = email {
        let doc = query::find_by_email(&conn, collection, &def, &email)?
            .ok_or_else(|| anyhow!("No user found with email '{}' in '{}'", email, collection))?;

        Ok((def, doc))
    } else if let Some(id) = id {
        let doc = query::find_by_id(&conn, collection, &def, &id, None)?
            .ok_or_else(|| anyhow!("No user found with id '{}' in '{}'", id, collection))?;

        Ok((def, doc))
    } else {
        // Interactive: select from existing users
        let find_query = query::FindQuery::default();
        let users = query::find(&conn, collection, &def, &find_query, None)?;

        if users.is_empty() {
            bail!("No users in '{}'", collection);
        }

        let labels: Vec<String> = users
            .iter()
            .map(|u| {
                let email = u
                    .fields
                    .get("email")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-");
                format!("{} — {}", email, u.id)
            })
            .collect();

        if users.len() == 1 {
            cli::info(&format!("Auto-selected only user: {}", labels[0]));
            let doc = users.into_iter().next().expect("guarded by len == 1");

            return Ok((def, doc));
        }

        let selection = Select::with_theme(&crap_theme())
            .with_prompt("Select user")
            .items(&labels)
            .interact()
            .context("Failed to read user selection")?;

        Ok((
            def,
            users
                .into_iter()
                .nth(selection)
                .expect("selection within bounds"),
        ))
    }
}

/// Create a new user in an auth collection.
/// Untestable: interactive email/password prompts via stdin and rpassword.
#[cfg(not(tarpaulin_include))]
pub fn user_create(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    password: Option<String>,
    fields: Vec<(String, String)>,
    password_policy: &PasswordPolicy,
) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg
        .get_collection(collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found", collection))?;

    if !def.is_auth_collection() {
        bail!(
            "Collection '{}' is not an auth collection (auth must be enabled)",
            collection
        );
    }

    let def = def.clone();

    drop(reg);

    // Get email — from flag or interactive prompt
    let email = match email {
        Some(e) => e,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Email")
            .interact_text()
            .context("Failed to read email")?,
    };

    // Get password — from flag or interactive prompt
    let password = match password {
        Some(p) => {
            cli::warning("Password provided via command line — it may be visible in shell history");
            p
        }
        None => Password::with_theme(&crap_theme())
            .with_prompt("Password")
            .with_confirmation("Confirm password", "Passwords do not match")
            .interact()
            .context("Failed to read password")?,
    };

    // Validate password against policy
    password_policy.validate(&password)?;

    // Build data map from email + extra --field args
    let mut data: HashMap<String, String> = fields.into_iter().collect();
    data.insert("email".to_string(), email);

    // Prompt for any required fields not already provided
    for field in &def.fields {
        if field.name == "email" {
            continue; // already handled above
        }
        if field.field_type == FieldType::Checkbox {
            continue; // absent checkbox = false, always valid
        }
        if data.contains_key(&field.name) {
            continue; // already provided via --field
        }
        if !field.required && field.default_value.is_none() {
            continue; // optional with no default — skip
        }
        // Use default_value if available and field is not required
        if !field.required
            && let Some(ref dv) = field.default_value
        {
            let val = match dv {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            data.insert(field.name.clone(), val);

            continue;
        }
        // Required field with a default — use it automatically
        if let Some(ref dv) = field.default_value {
            let val = match dv {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            let entered: String = Input::with_theme(&crap_theme())
                .with_prompt(format!("{} (required)", field.name))
                .default(val)
                .interact_text()
                .with_context(|| format!("Failed to read {}", field.name))?;

            data.insert(field.name.clone(), entered);

            continue;
        }

        // Required field, no default — must prompt
        let entered: String = Input::with_theme(&crap_theme())
            .with_prompt(format!("{} (required)", field.name))
            .interact_text()
            .with_context(|| format!("Failed to read {}", field.name))?;

        if entered.is_empty() {
            bail!("{} is required", field.name);
        }

        data.insert(field.name.clone(), entered);
    }

    // Create user in a transaction
    let mut conn = pool.get().context("Failed to get database connection")?;
    let tx = conn.transaction().context("Failed to begin transaction")?;

    let doc = query::create(&tx, collection, &def, &data, None).context("Failed to create user")?;

    query::update_password(&tx, collection, &doc.id, &password)
        .context("Failed to set password")?;

    tx.commit().context("Failed to commit transaction")?;

    cli::success(&format!("Created user {} in '{}'", doc.id, collection));

    Ok(())
}

/// List users in an auth collection.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_list(pool: &DbPool, registry: &SharedRegistry, collection: &str) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg
        .get_collection(collection)
        .ok_or_else(|| anyhow!("Collection '{}' not found", collection))?;

    if !def.is_auth_collection() {
        bail!(
            "Collection '{}' is not an auth collection (auth must be enabled)",
            collection
        );
    }

    let def = def.clone();

    let verify_email = def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false);

    drop(reg);

    let conn = pool.get().context("Failed to get database connection")?;

    let find_query = query::FindQuery::default();

    let users = query::find(&conn, collection, &def, &find_query, None)?;

    if users.is_empty() {
        cli::info(&format!("No users in '{}'.", collection));

        return Ok(());
    }

    let mut table = if verify_email {
        Table::new(vec!["ID", "Email", "Locked", "Verified"])
    } else {
        Table::new(vec!["ID", "Email", "Locked"])
    };

    for user in &users {
        let email = user
            .fields
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let locked = query::is_locked(&conn, collection, &user.id).unwrap_or(false);
        let locked_str = if locked { "yes" } else { "no" };

        if verify_email {
            let verified = query::is_verified(&conn, collection, &user.id).unwrap_or(false);
            let verified_str = if verified { "yes" } else { "no" };

            table.row(vec![&user.id, email, locked_str, verified_str]);
        } else {
            table.row(vec![&user.id, email, locked_str]);
        }
    }

    table.print();
    table.footer(&format!("{} user(s)", users.len()));

    Ok(())
}

/// Show detailed info for a single user.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_info(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

    let verify_email = def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false);

    let conn = pool.get().context("Failed to get database connection")?;

    let locked = query::is_locked(&conn, collection, &doc.id).unwrap_or(false);
    let has_pw = query::has_password(&conn, collection, &doc.id).unwrap_or(false);

    let user_email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("-");

    cli::kv("User", &doc.id);
    cli::kv("Collection", collection);
    cli::kv("Email", user_email);

    cli::header("Status");
    cli::kv_status("Locked", if locked { "yes" } else { "no" }, !locked);

    if verify_email {
        let verified = query::is_verified(&conn, collection, &doc.id).unwrap_or(false);

        cli::kv_status("Verified", if verified { "yes" } else { "no" }, verified);
    }

    cli::kv_status("Password", if has_pw { "set" } else { "not set" }, has_pw);

    // Timestamps
    let created = doc
        .fields
        .get("created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("-");

    let updated = doc
        .fields
        .get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("-");

    cli::header("Timestamps");
    cli::kv("Created", created);
    cli::kv("Updated", updated);

    // Extra fields (skip email, created_at, updated_at — already shown)
    let skip = ["email", "created_at", "updated_at"];
    let extra: Vec<_> = doc
        .fields
        .iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .collect();

    if !extra.is_empty() {
        cli::header("Fields");

        for (key, val) in &extra {
            let display = match val {
                Value::String(s) => s.clone(),
                Value::Null => "-".to_string(),
                other => other.to_string(),
            };

            cli::kv(key, &display);
        }
    }

    Ok(())
}

/// Delete a user from an auth collection.
/// Untestable: interactive confirmation via dialoguer::Confirm.
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

    let user_email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

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

    let conn = pool.get().context("Failed to get database connection")?;
    query::delete(&conn, collection, &doc.id).context("Failed to delete user")?;

    cli::success(&format!(
        "Deleted user {} ({}) from '{}'",
        doc.id, user_email, collection
    ));

    Ok(())
}

/// Lock a user account.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
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
    query::lock_user(&conn, collection, &doc.id).context("Failed to lock user")?;

    let user_email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    cli::success(&format!(
        "Locked user {} ({}) in '{}'",
        doc.id, user_email, collection
    ));

    Ok(())
}

/// Unlock a user account.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
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
    query::unlock_user(&conn, collection, &doc.id).context("Failed to unlock user")?;

    let user_email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    cli::success(&format!(
        "Unlocked user {} ({}) in '{}'",
        doc.id, user_email, collection
    ));

    Ok(())
}

/// Verify a user account (mark email as verified).
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_verify(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

    if !def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false) {
        bail!(
            "Collection '{}' does not have email verification enabled (verify_email must be true)",
            collection
        );
    }

    let conn = pool.get().context("Failed to get database connection")?;
    query::mark_verified(&conn, collection, &doc.id).context("Failed to verify user")?;

    let user_email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    cli::success(&format!(
        "Verified user {} ({}) in '{}'",
        doc.id, user_email, collection
    ));

    Ok(())
}

/// Unverify a user account (mark email as unverified).
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_unverify(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

    if !def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false) {
        bail!(
            "Collection '{}' does not have email verification enabled (verify_email must be true)",
            collection
        );
    }

    let conn = pool.get().context("Failed to get database connection")?;
    query::mark_unverified(&conn, collection, &doc.id).context("Failed to unverify user")?;

    let user_email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    cli::success(&format!(
        "Unverified user {} ({}) in '{}'",
        doc.id, user_email, collection
    ));

    Ok(())
}

/// Change a user's password.
/// Untestable: interactive password prompts via rpassword + depends on resolve_user.
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

    // Validate password against policy
    password_policy.validate(&password)?;

    let conn = pool.get().context("Failed to get database connection")?;
    query::update_password(&conn, collection, &doc.id, &password)
        .context("Failed to update password")?;

    let user_email = doc
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    cli::success(&format!(
        "Password changed for user {} ({}) in '{}'",
        doc.id, user_email, collection
    ));

    Ok(())
}
