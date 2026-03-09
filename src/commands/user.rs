//! `user` command — user management for auth collections.

use anyhow::{Context as _, Result};
use std::collections::HashMap;


/// Dispatch user management subcommands.
/// Untestable: dispatches to interactive CLI functions that require stdin/dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn run(action: super::UserAction) -> Result<()> {
    match action {
        super::UserAction::Create { config, collection, email, password, fields } => {
            let cfg = crate::config::CrapConfig::load(&config)
                .context("Failed to load config")?;
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_create(&pool, &registry, &collection, email, password, fields, &cfg.auth.password_policy)
        }
        super::UserAction::List { config, collection } => {
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_list(&pool, &registry, &collection)
        }
        super::UserAction::Info { config, collection, email, id } => {
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_info(&pool, &registry, &collection, email, id)
        }
        super::UserAction::Delete { config, collection, email, id, confirm } => {
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_delete(&pool, &registry, &collection, email, id, confirm)
        }
        super::UserAction::Lock { config, collection, email, id } => {
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_lock(&pool, &registry, &collection, email, id)
        }
        super::UserAction::Unlock { config, collection, email, id } => {
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_unlock(&pool, &registry, &collection, email, id)
        }
        super::UserAction::Verify { config, collection, email, id } => {
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_verify(&pool, &registry, &collection, email, id)
        }
        super::UserAction::Unverify { config, collection, email, id } => {
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_unverify(&pool, &registry, &collection, email, id)
        }
        super::UserAction::ChangePassword { config, collection, email, id, password } => {
            let cfg = crate::config::CrapConfig::load(&config)
                .context("Failed to load config")?;
            let (pool, registry) = super::load_config_and_sync(&config)?;
            user_change_password(&pool, &registry, &collection, email, id, password, &cfg.auth.password_policy)
        }
    }
}

/// Resolve a user by --email or --id. Returns (def, document).
/// Untestable: interactive fallback uses dialoguer::Select for user selection.
#[cfg(not(tarpaulin_include))]
fn resolve_user(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<(crate::core::CollectionDefinition, crate::core::Document)> {
    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg.get_collection(collection)
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", collection))?;

    if !def.is_auth_collection() {
        anyhow::bail!("Collection '{}' is not an auth collection (auth must be enabled)", collection);
    }

    let def = def.clone();
    drop(reg);

    let conn = pool.get().context("Failed to get database connection")?;

    if let Some(email) = email {
        let doc = crate::db::query::find_by_email(&conn, collection, &def, &email)?
            .ok_or_else(|| anyhow::anyhow!("No user found with email '{}' in '{}'", email, collection))?;
        Ok((def, doc))
    } else if let Some(id) = id {
        let doc = crate::db::query::find_by_id(&conn, collection, &def, &id, None)?
            .ok_or_else(|| anyhow::anyhow!("No user found with id '{}' in '{}'", id, collection))?;
        Ok((def, doc))
    } else {
        // Interactive: select from existing users
        use dialoguer::Select;
        let query = crate::db::query::FindQuery::default();
        let users = crate::db::query::find(&conn, collection, &def, &query, None)?;
        if users.is_empty() {
            anyhow::bail!("No users in '{}'", collection);
        }
        let labels: Vec<String> = users.iter().map(|u| {
            let email = u.fields.get("email").and_then(|v| v.as_str()).unwrap_or("-");
            format!("{} — {}", email, u.id)
        }).collect();
        if users.len() == 1 {
            println!("Auto-selected only user: {}", labels[0]);
            let doc = users.into_iter().next().expect("guarded by len == 1");
            return Ok((def, doc));
        }
        let selection = Select::new()
            .with_prompt("Select user")
            .items(&labels)
            .interact()
            .context("Failed to read user selection")?;
        Ok((def, users.into_iter().nth(selection).expect("selection within bounds")))
    }
}

/// Create a new user in an auth collection.
/// Untestable: interactive email/password prompts via stdin and rpassword.
#[cfg(not(tarpaulin_include))]
pub fn user_create(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    password: Option<String>,
    fields: Vec<(String, String)>,
    password_policy: &crate::config::PasswordPolicy,
) -> Result<()> {
    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg.get_collection(collection)
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", collection))?;

    if !def.is_auth_collection() {
        anyhow::bail!("Collection '{}' is not an auth collection (auth must be enabled)", collection);
    }

    let def = def.clone();
    drop(reg);

    // Get email — from flag or interactive prompt
    let email = match email {
        Some(e) => e,
        None => {
            eprint!("Email: ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)
                .context("Failed to read email")?;
            let trimmed = input.trim().to_string();
            if trimmed.is_empty() {
                anyhow::bail!("Email cannot be empty");
            }
            trimmed
        }
    };

    // Get password — from flag or interactive prompt
    let password = match password {
        Some(p) => {
            eprintln!("Warning: password provided via command line — it may be visible in shell history");
            p
        }
        None => {
            eprint!("Password: ");
            let p1 = rpassword::read_password()
                .context("Failed to read password")?;
            if p1.is_empty() {
                anyhow::bail!("Password cannot be empty");
            }
            eprint!("Confirm password: ");
            let p2 = rpassword::read_password()
                .context("Failed to read password confirmation")?;
            if p1 != p2 {
                anyhow::bail!("Passwords do not match");
            }
            p1
        }
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
        if field.field_type == crate::core::field::FieldType::Checkbox {
            continue; // absent checkbox = false, always valid
        }
        if data.contains_key(&field.name) {
            continue; // already provided via --field
        }
        if !field.required && field.default_value.is_none() {
            continue; // optional with no default — skip
        }
        // Use default_value if available and field is not required
        if !field.required {
            if let Some(ref dv) = field.default_value {
                let val = match dv {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                data.insert(field.name.clone(), val);
                continue;
            }
        }
        // Required field with a default — use it automatically
        if let Some(ref dv) = field.default_value {
            let val = match dv {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            eprint!("{} [{}]: ", field.name, val);
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)
                .with_context(|| format!("Failed to read {}", field.name))?;
            let trimmed = input.trim();
            if trimmed.is_empty() {
                data.insert(field.name.clone(), val);
            } else {
                data.insert(field.name.clone(), trimmed.to_string());
            }
            continue;
        }
        // Required field, no default — must prompt
        eprint!("{}: ", field.name);
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)
            .with_context(|| format!("Failed to read {}", field.name))?;
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            anyhow::bail!("{} is required", field.name);
        }
        data.insert(field.name.clone(), trimmed);
    }

    // Create user in a transaction
    let mut conn = pool.get().context("Failed to get database connection")?;
    let tx = conn.transaction().context("Failed to begin transaction")?;

    let doc = crate::db::query::create(&tx, collection, &def, &data, None)
        .context("Failed to create user")?;

    crate::db::query::update_password(&tx, collection, &doc.id, &password)
        .context("Failed to set password")?;

    tx.commit().context("Failed to commit transaction")?;

    println!("Created user {} in '{}'", doc.id, collection);

    Ok(())
}

/// List users in an auth collection.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_list(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
) -> Result<()> {
    let reg = registry.read()
        .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;

    let def = reg.get_collection(collection)
        .ok_or_else(|| anyhow::anyhow!("Collection '{}' not found", collection))?;

    if !def.is_auth_collection() {
        anyhow::bail!("Collection '{}' is not an auth collection (auth must be enabled)", collection);
    }

    let def = def.clone();
    let verify_email = def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false);
    drop(reg);

    let conn = pool.get().context("Failed to get database connection")?;

    let query = crate::db::query::FindQuery::default();

    let users = crate::db::query::find(&conn, collection, &def, &query, None)?;

    if users.is_empty() {
        println!("No users in '{}'.", collection);
        return Ok(());
    }

    // Print header
    if verify_email {
        println!("{:<24} {:<30} {:<8} {:<8}", "ID", "Email", "Locked", "Verified");
        println!("{}", "-".repeat(72));
    } else {
        println!("{:<24} {:<30} {:<8}", "ID", "Email", "Locked");
        println!("{}", "-".repeat(64));
    }

    for user in &users {
        let email = user.fields.get("email")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let locked = crate::db::query::is_locked(&conn, collection, &user.id).unwrap_or(false);
        let locked_str = if locked { "yes" } else { "no" };

        if verify_email {
            let verified = crate::db::query::is_verified(&conn, collection, &user.id).unwrap_or(false);
            let verified_str = if verified { "yes" } else { "no" };
            println!("{:<24} {:<30} {:<8} {:<8}", user.id, email, locked_str, verified_str);
        } else {
            println!("{:<24} {:<30} {:<8}", user.id, email, locked_str);
        }
    }

    println!("\n{} user(s)", users.len());

    Ok(())
}

/// Show detailed info for a single user.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_info(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

    let verify_email = def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false);

    let conn = pool.get().context("Failed to get database connection")?;

    let locked = crate::db::query::is_locked(&conn, collection, &doc.id).unwrap_or(false);
    let has_pw = crate::db::query::has_password(&conn, collection, &doc.id).unwrap_or(false);

    let user_email = doc.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("-");

    println!("User: {}", doc.id);
    println!("Collection: {}", collection);
    println!("Email: {}", user_email);

    println!("\nStatus:");
    println!("  Locked:    {}", if locked { "yes" } else { "no" });
    if verify_email {
        let verified = crate::db::query::is_verified(&conn, collection, &doc.id).unwrap_or(false);
        println!("  Verified:  {}", if verified { "yes" } else { "no" });
    }
    println!("  Password:  {}", if has_pw { "set" } else { "not set" });

    // Timestamps
    let created = doc.fields.get("created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    let updated = doc.fields.get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    println!("\nTimestamps:");
    println!("  Created:   {}", created);
    println!("  Updated:   {}", updated);

    // Extra fields (skip email, created_at, updated_at — already shown)
    let skip = ["email", "created_at", "updated_at"];
    let extra: Vec<_> = doc.fields.iter()
        .filter(|(k, _)| !skip.contains(&k.as_str()))
        .collect();

    if !extra.is_empty() {
        println!("\nFields:");
        for (key, val) in &extra {
            let display = match val {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => "-".to_string(),
                other => other.to_string(),
            };
            println!("  {:<12} {}", format!("{}:", key), display);
        }
    }

    Ok(())
}

/// Delete a user from an auth collection.
/// Untestable: interactive confirmation via dialoguer::Confirm.
#[cfg(not(tarpaulin_include))]
pub fn user_delete(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
    confirm: bool,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

    let user_email = doc.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if !confirm {
        use dialoguer::Confirm;
        let proceed = Confirm::new()
            .with_prompt(format!("Delete user {} ({}) from '{}'?", doc.id, user_email, collection))
            .default(false)
            .interact()
            .context("Failed to read confirmation")?;
        if !proceed {
            println!("Aborted.");
            return Ok(());
        }
    }

    let conn = pool.get().context("Failed to get database connection")?;
    crate::db::query::delete(&conn, collection, &doc.id)
        .context("Failed to delete user")?;

    println!("Deleted user {} ({}) from '{}'", doc.id, user_email, collection);

    Ok(())
}

/// Lock a user account.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_lock(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

    let conn = pool.get().context("Failed to get database connection")?;
    crate::db::query::lock_user(&conn, collection, &doc.id)
        .context("Failed to lock user")?;

    let user_email = doc.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("Locked user {} ({}) in '{}'", doc.id, user_email, collection);

    Ok(())
}

/// Unlock a user account.
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_unlock(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

    let conn = pool.get().context("Failed to get database connection")?;
    crate::db::query::unlock_user(&conn, collection, &doc.id)
        .context("Failed to unlock user")?;

    let user_email = doc.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("Unlocked user {} ({}) in '{}'", doc.id, user_email, collection);

    Ok(())
}

/// Verify a user account (mark email as verified).
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_verify(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

    if !def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false) {
        anyhow::bail!("Collection '{}' does not have email verification enabled (verify_email must be true)", collection);
    }

    let conn = pool.get().context("Failed to get database connection")?;
    crate::db::query::mark_verified(&conn, collection, &doc.id)
        .context("Failed to verify user")?;

    let user_email = doc.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("Verified user {} ({}) in '{}'", doc.id, user_email, collection);

    Ok(())
}

/// Unverify a user account (mark email as unverified).
/// Untestable: depends on resolve_user which uses interactive dialoguer.
#[cfg(not(tarpaulin_include))]
pub fn user_unverify(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<()> {
    let (def, doc) = resolve_user(pool, registry, collection, email, id)?;

    if !def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false) {
        anyhow::bail!("Collection '{}' does not have email verification enabled (verify_email must be true)", collection);
    }

    let conn = pool.get().context("Failed to get database connection")?;
    crate::db::query::mark_unverified(&conn, collection, &doc.id)
        .context("Failed to unverify user")?;

    let user_email = doc.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("Unverified user {} ({}) in '{}'", doc.id, user_email, collection);

    Ok(())
}

/// Change a user's password.
/// Untestable: interactive password prompts via rpassword + depends on resolve_user.
#[cfg(not(tarpaulin_include))]
pub fn user_change_password(
    pool: &crate::db::DbPool,
    registry: &crate::core::SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
    password: Option<String>,
    password_policy: &crate::config::PasswordPolicy,
) -> Result<()> {
    let (_, doc) = resolve_user(pool, registry, collection, email, id)?;

    let password = match password {
        Some(p) => {
            eprintln!("Warning: password provided via command line — it may be visible in shell history");
            p
        }
        None => {
            eprint!("New password: ");
            let p1 = rpassword::read_password()
                .context("Failed to read password")?;
            if p1.is_empty() {
                anyhow::bail!("Password cannot be empty");
            }
            eprint!("Confirm password: ");
            let p2 = rpassword::read_password()
                .context("Failed to read password confirmation")?;
            if p1 != p2 {
                anyhow::bail!("Passwords do not match");
            }
            p1
        }
    };

    // Validate password against policy
    password_policy.validate(&password)?;

    let conn = pool.get().context("Failed to get database connection")?;
    crate::db::query::update_password(&conn, collection, &doc.id, &password)
        .context("Failed to update password")?;

    let user_email = doc.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("Password changed for user {} ({}) in '{}'", doc.id, user_email, collection);

    Ok(())
}
