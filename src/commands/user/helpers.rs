//! Shared helpers for user management commands.

use anyhow::{Context as _, Result, anyhow, bail};
use dialoguer::Select;
use serde_json::Value;
use std::collections::HashMap;

use crate::{
    cli::{self, crap_theme},
    core::{CollectionDefinition, Document, SharedRegistry, field::FieldType},
    db::{BoxedConnection, DbPool, query},
};

#[cfg(not(tarpaulin_include))]
use dialoguer::Input;

/// Extract the email field from a user document, defaulting to "unknown".
pub fn get_user_email(doc: &Document) -> &str {
    doc.fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

/// Load and validate an auth collection definition from the registry.
/// Returns the cloned definition (lock is released before returning).
pub fn load_auth_collection(
    registry: &SharedRegistry,
    collection: &str,
) -> Result<CollectionDefinition> {
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

    Ok(def.clone())
}

/// Check that the collection has email verification enabled.
pub fn require_verify_email(def: &CollectionDefinition, collection: &str) -> Result<()> {
    if !def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false) {
        bail!(
            "Collection '{}' does not have email verification enabled (verify_email must be true)",
            collection
        );
    }

    Ok(())
}

/// Resolve a user by --email or --id. Returns (def, document).
/// Untestable: interactive fallback uses dialoguer::Select for user selection.
#[cfg(not(tarpaulin_include))]
pub fn resolve_user(
    pool: &DbPool,
    registry: &SharedRegistry,
    collection: &str,
    email: Option<String>,
    id: Option<String>,
) -> Result<(CollectionDefinition, Document)> {
    let def = load_auth_collection(registry, collection)?;
    let conn = pool.get().context("Failed to get database connection")?;

    if let Some(email) = email {
        let doc = query::find_by_email(&conn, collection, &def, &email)?
            .ok_or_else(|| anyhow!("No user found with email '{}' in '{}'", email, collection))?;

        return Ok((def, doc));
    }

    if let Some(id) = id {
        let doc = query::find_by_id(&conn, collection, &def, &id, None)?
            .ok_or_else(|| anyhow!("No user found with id '{}' in '{}'", id, collection))?;

        return Ok((def, doc));
    }

    // Interactive: select from existing users
    select_user_interactive(&conn, collection, &def)
}

/// Interactively select a user from the collection.
#[cfg(not(tarpaulin_include))]
fn select_user_interactive(
    conn: &BoxedConnection,
    collection: &str,
    def: &CollectionDefinition,
) -> Result<(CollectionDefinition, Document)> {
    let find_query = query::FindQuery::default();
    let users = query::find(conn, collection, def, &find_query, None)?;

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

        return Ok((def.clone(), doc));
    }

    let selection = Select::with_theme(&crap_theme())
        .with_prompt("Select user")
        .items(&labels)
        .interact()
        .context("Failed to read user selection")?;

    Ok((
        def.clone(),
        users
            .into_iter()
            .nth(selection)
            .expect("selection within bounds"),
    ))
}

/// Convert a JSON default value to a string.
pub fn default_value_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Prompt for required fields not already present in the data map.
/// Skips email (handled separately) and checkboxes (absent = false).
#[cfg(not(tarpaulin_include))]
pub fn prompt_required_fields(
    def: &CollectionDefinition,
    data: &mut HashMap<String, String>,
) -> Result<()> {
    for field in &def.fields {
        if field.name == "email" || field.field_type == FieldType::Checkbox {
            continue;
        }

        if data.contains_key(&field.name) {
            continue;
        }

        if !field.required && field.default_value.is_none() {
            continue;
        }

        // Optional with default — insert silently
        if !field.required
            && let Some(ref dv) = field.default_value
        {
            data.insert(field.name.clone(), default_value_string(dv));
            continue;
        }

        // Required with default — prompt with prefilled value
        if let Some(ref dv) = field.default_value {
            let entered: String = Input::with_theme(&crap_theme())
                .with_prompt(format!("{} (required)", field.name))
                .default(default_value_string(dv))
                .interact_text()
                .with_context(|| format!("Failed to read {}", field.name))?;

            data.insert(field.name.clone(), entered);
            continue;
        }

        // Required, no default — must prompt
        let entered: String = Input::with_theme(&crap_theme())
            .with_prompt(format!("{} (required)", field.name))
            .interact_text()
            .with_context(|| format!("Failed to read {}", field.name))?;

        if entered.is_empty() {
            bail!("{} is required", field.name);
        }

        data.insert(field.name.clone(), entered);
    }

    Ok(())
}
