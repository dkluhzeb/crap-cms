//! `user info` — show detailed info for a single user.

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::{
    cli,
    core::{Document, SharedRegistry},
    db::{BoxedConnection, DbPool, query},
};

use super::helpers::{get_user_email, resolve_user};

/// Show detailed info for a single user.
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

    print_identity(&doc, collection);
    print_status(&conn, collection, &doc, locked, has_pw, verify_email);
    print_timestamps(&doc);
    print_extra_fields(&doc);

    Ok(())
}

/// Print user identity (ID, collection, email).
fn print_identity(doc: &Document, collection: &str) {
    cli::kv("User", &doc.id);
    cli::kv("Collection", collection);
    cli::kv("Email", get_user_email(doc));
}

/// Print user status (locked, verified, password).
fn print_status(
    conn: &BoxedConnection,
    collection: &str,
    doc: &Document,
    locked: bool,
    has_pw: bool,
    verify_email: bool,
) {
    cli::header("Status");
    cli::kv_status("Locked", if locked { "yes" } else { "no" }, !locked);

    if verify_email {
        let verified = query::is_verified(conn, collection, &doc.id).unwrap_or(false);
        cli::kv_status("Verified", if verified { "yes" } else { "no" }, verified);
    }

    cli::kv_status("Password", if has_pw { "set" } else { "not set" }, has_pw);
}

/// Print created/updated timestamps.
fn print_timestamps(doc: &Document) {
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
}

/// Print extra fields (skip email, timestamps — already shown).
fn print_extra_fields(doc: &Document) {
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
}
