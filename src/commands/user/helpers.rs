//! Shared helpers for user management commands.

use std::{
    collections::HashMap,
    io::{BufRead, stdin},
    sync::Arc,
};

use anyhow::{Context as _, Result, anyhow, bail};
use dialoguer::{Password, Select};
use serde_json::Value;

use crate::{
    cli::{self, crap_theme},
    commands::cli_find,
    config::LocaleConfig,
    core::{CollectionDefinition, Document, Registry, collection::Auth, field::FieldType},
    db::{BoxedConnection, DbPool, FindQuery, LocaleContext, query},
};
#[cfg(not(tarpaulin_include))]
use dialoguer::Input;

/// Read a password from the first line of `reader`, without its line ending.
///
/// # Errors
///
/// Returns an error if reading fails or the line is empty.
pub(super) fn password_from_reader(mut reader: impl BufRead) -> Result<String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .context("Failed to read password from standard input")?;

    let password = line.trim_end_matches(['\n', '\r']);
    if password.is_empty() {
        bail!("No password on standard input");
    }

    Ok(password.to_string())
}

/// Resolve a new password: standard input when `from_stdin`, else the
/// `-p` value (with a visibility warning), else an interactive prompt with
/// confirmation.
///
/// # Errors
///
/// Returns an error if standard input or the prompt cannot be read.
#[cfg(not(tarpaulin_include))]
pub(super) fn resolve_new_password(
    password: Option<String>,
    from_stdin: bool,
    prompt: &str,
) -> Result<String> {
    if from_stdin {
        return password_from_reader(stdin().lock());
    }

    if let Some(p) = password {
        cli::warning(
            "Password provided via command line — it is visible to other local users and kept \
             in shell history; use --password-stdin instead",
        );
        return Ok(p);
    }

    Password::with_theme(&crap_theme())
        .with_prompt(prompt)
        .with_confirmation("Confirm password", "Passwords do not match")
        .interact()
        .context("Failed to read password")
}

/// Extract the email field from a user document, defaulting to "unknown".
pub(super) fn get_user_email(doc: &Document) -> &str {
    doc.fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

/// Load and validate an auth collection definition from the registry.
/// Returns the cloned definition (lock is released before returning).
pub(super) fn load_auth_collection(
    registry: &Registry,
    collection: &str,
) -> Result<Arc<CollectionDefinition>> {
    let def = registry
        .get_collection(collection)
        .ok_or_else(|| anyhow!("Collection '{collection}' not found"))?;

    if !def.is_auth_collection() {
        bail!("Collection '{collection}' is not an auth collection (auth must be enabled)");
    }

    Ok(def.clone())
}

/// Check that the collection has email verification enabled.
pub(super) fn require_verify_email(def: &CollectionDefinition, collection: &str) -> Result<()> {
    if !def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
        bail!(
            "Collection '{collection}' does not have email verification enabled (verify_email must be true)"
        );
    }

    Ok(())
}

/// What every user subcommand needs to find the user it operates on.
/// `locale` is required because a LOCALIZED auth collection's rows can only
/// be selected under a locale context (bare column names don't exist there).
pub struct UserLookup<'a> {
    pub pool: &'a DbPool,
    pub registry: &'a Registry,
    pub collection: &'a str,
    pub email: Option<String>,
    pub id: Option<String>,
    pub locale: &'a LocaleConfig,
}

/// Resolve a user by --email or --id. Returns (def, document).
/// Untestable: interactive fallback uses `dialoguer::Select` for user selection.
#[cfg(not(tarpaulin_include))]
pub(super) fn resolve_user(
    lookup: &UserLookup<'_>,
) -> Result<(Arc<CollectionDefinition>, Document)> {
    let UserLookup {
        pool,
        registry,
        collection,
        email,
        id,
        locale,
    } = lookup;
    let (email, id) = (email.clone(), id.clone());
    let def = load_auth_collection(registry, collection)?;
    let conn = pool.get().context("Failed to get database connection")?;
    let locale_ctx = LocaleContext::default_for(locale);

    if let Some(email) = email {
        // Admin tooling reaches soft-deleted users too (e.g. password recovery
        // for a trashed account) — unlike the HTTP login/reset paths, which
        // exclude them.
        let doc = query::find_by_email(&conn, collection, &def, &email, true, locale_ctx.as_ref())?
            .ok_or_else(|| anyhow!("No user found with email '{email}' in '{collection}'"))?;

        return Ok((def, doc));
    }

    if let Some(id) = id {
        let doc = query::find_by_id(&conn, collection, &def, &id, locale_ctx.as_ref())?
            .ok_or_else(|| anyhow!("No user found with id '{id}' in '{collection}'"))?;

        return Ok((def, doc));
    }

    // Interactive: select from existing users
    select_user_interactive(&conn, collection, &def, locale)
}

/// Interactively select a user from the collection.
#[cfg(not(tarpaulin_include))]
fn select_user_interactive(
    conn: &BoxedConnection,
    collection: &str,
    def: &Arc<CollectionDefinition>,
    locale: &LocaleConfig,
) -> Result<(Arc<CollectionDefinition>, Document)> {
    let users = cli_find(conn, def, &FindQuery::default(), locale)?;

    if users.is_empty() {
        bail!("No users in '{collection}'");
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

    let user = users
        .into_iter()
        .nth(selection)
        .context("Selected user index out of bounds")?;

    Ok((def.clone(), user))
}

/// Convert a JSON default value to a string.
pub(super) fn default_value_string(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Prompt for required fields not already present in the data map.
/// Skips email (handled separately) and checkboxes (absent = false).
#[cfg(not(tarpaulin_include))]
pub(super) fn prompt_required_fields(
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use crate::core::document::DocumentBuilder;

    use std::io::Cursor;

    use super::*;

    #[test]
    fn get_user_email_returns_email_or_unknown() {
        let map: HashMap<String, Value> =
            serde_json::from_value(json!({ "email": "a@example.com" })).unwrap();
        let with_email = DocumentBuilder::new("u1").fields(map).build();
        assert_eq!(get_user_email(&with_email), "a@example.com");

        let without = DocumentBuilder::new("u2").build();
        assert_eq!(get_user_email(&without), "unknown");
    }

    #[test]
    fn default_value_string_unquotes_strings_but_renders_others_as_json() {
        assert_eq!(default_value_string(&json!("hello")), "hello"); // no surrounding quotes
        assert_eq!(default_value_string(&json!(42)), "42");
        assert_eq!(default_value_string(&json!(true)), "true");
        assert_eq!(default_value_string(&Value::Null), "null");
    }

    /// Only the first line is the password; its line ending is not part of it.
    #[test]
    fn password_from_reader_takes_the_first_line_without_its_ending() {
        let read = |input: &str| password_from_reader(Cursor::new(input.as_bytes()));

        assert_eq!(read("s3cret pass\n").unwrap(), "s3cret pass");
        assert_eq!(read("s3cret\r\nignored\n").unwrap(), "s3cret");
        assert_eq!(read("no-newline").unwrap(), "no-newline");
        assert!(read("\n").is_err());
        assert!(read("").is_err());
    }
}
