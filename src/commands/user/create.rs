//! `user create` — create a new user in an auth collection.

use anyhow::{Context as _, Result};
use dialoguer::{Input, Password};
use std::collections::HashMap;

use crate::{
    cli::{self, crap_theme},
    config::PasswordPolicy,
    core::SharedRegistry,
    db::{DbPool, query},
};

use super::helpers::{load_auth_collection, prompt_required_fields};

/// Create a new user in an auth collection.
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
    let def = load_auth_collection(registry, collection)?;

    let email = resolve_email(email)?;
    let password = resolve_password(password)?;

    password_policy.validate(&password)?;

    let mut data: HashMap<String, String> = fields.into_iter().collect();
    data.insert("email".to_string(), email);

    prompt_required_fields(&def, &mut data)?;

    let mut conn = pool.get().context("Failed to get database connection")?;
    let tx = conn.transaction().context("Failed to begin transaction")?;

    let doc = query::create(&tx, collection, &def, &data, None).context("Failed to create user")?;

    query::update_password(&tx, collection, &doc.id, &password)
        .context("Failed to set password")?;

    tx.commit().context("Failed to commit transaction")?;

    cli::success(&format!("Created user {} in '{}'", doc.id, collection));

    Ok(())
}

/// Resolve email from CLI flag or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_email(email: Option<String>) -> Result<String> {
    match email {
        Some(e) => Ok(e),
        None => Input::with_theme(&crap_theme())
            .with_prompt("Email")
            .interact_text()
            .context("Failed to read email"),
    }
}

/// Resolve password from CLI flag or interactive prompt.
#[cfg(not(tarpaulin_include))]
fn resolve_password(password: Option<String>) -> Result<String> {
    match password {
        Some(p) => {
            cli::warning("Password provided via command line — it may be visible in shell history");
            Ok(p)
        }
        None => Password::with_theme(&crap_theme())
            .with_prompt("Password")
            .with_confirmation("Confirm password", "Passwords do not match")
            .interact()
            .context("Failed to read password"),
    }
}
