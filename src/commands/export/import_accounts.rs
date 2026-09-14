//! The accounts an import writes: the sessions their imported credentials
//! replace, and whether they are left without a password.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    commands::export::import_row::ImportTarget,
    core::collection::Auth,
    db::{DbConnection, query},
};

/// The session version of the stored account imported credentials replace —
/// `None` when the document carries no credentials or no such account exists.
pub(super) fn replaced_session_version(
    tx: &dyn DbConnection,
    target: &ImportTarget<'_>,
    doc_obj: &Map<String, Value>,
    id: &str,
) -> Result<Option<u64>> {
    if !doc_obj.contains_key("_credentials")
        || !target.credential_columns.contains(&"_session_version")
        || !query::auth::user_exists(tx, target.slug, id)?
    {
        return Ok(None);
    }

    Ok(Some(query::auth::get_session_version(tx, target.slug, id)?))
}

/// Move an account's session version past both the one its credentials
/// replaced and the one the export carried, so no token the target issued —
/// or had already revoked — is accepted again.
pub(super) fn revoke_replaced_sessions(
    tx: &dyn DbConnection,
    slug: &str,
    id: &str,
    replaced: u64,
) -> Result<()> {
    let carried = query::auth::get_session_version(tx, slug, id)?;

    query::auth::set_session_version(tx, slug, id, replaced.max(carried).saturating_add(1))
}

/// Whether an account ends the import without a password it logs in with — only
/// a collection with password login has one to lack.
pub(super) fn lacks_password(
    tx: &dyn DbConnection,
    target: &ImportTarget<'_>,
    id: &str,
) -> Result<bool> {
    if !target
        .def
        .auth
        .as_ref()
        .is_some_and(Auth::password_login_enabled)
    {
        return Ok(false);
    }

    Ok(query::get_password_hash(tx, target.slug, id)?.is_none())
}
