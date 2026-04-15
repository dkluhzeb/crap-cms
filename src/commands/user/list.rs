//! `user list` — list users in an auth collection.

use anyhow::{Context as _, Result};

use crate::{
    cli::{self, Table},
    core::SharedRegistry,
    db::{DbPool, query},
    service::{self, ServiceContext},
};

use super::helpers::load_auth_collection;

/// List users in an auth collection.
#[cfg(not(tarpaulin_include))]
pub fn user_list(pool: &DbPool, registry: &SharedRegistry, collection: &str) -> Result<()> {
    let def = load_auth_collection(registry, collection)?;
    let verify_email = def.auth.as_ref().map(|a| a.verify_email).unwrap_or(false);

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

    let ctx = ServiceContext::slug_only(collection).conn(&conn).build();

    for user in &users {
        let email = user
            .fields
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let locked = service::auth::is_locked(&ctx, &user.id).unwrap_or(false);
        let locked_str = if locked { "yes" } else { "no" };

        if verify_email {
            let verified = service::auth::is_verified(&ctx, &user.id).unwrap_or(false);
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
