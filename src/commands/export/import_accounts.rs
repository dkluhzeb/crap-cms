//! The accounts an import writes: the sessions their imported credentials
//! replace, the accounts it overwrites, and whether they are left without a
//! password.

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

/// Whether the import overwrites an account that already exists. Its roles,
/// lock state or credentials may change, and a live-update stream resolves its
/// access once, at connect — so the stream must be torn down after the commit.
pub(super) fn overwrites_account(
    tx: &dyn DbConnection,
    target: &ImportTarget<'_>,
    id: &str,
) -> Result<bool> {
    if !target.def.is_auth_collection() {
        return Ok(false);
    }

    query::auth::user_exists(tx, target.slug, id)
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

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::{
        config::LocaleConfig,
        core::{CollectionDefinition, collection::Auth},
        db::InMemoryConn,
    };

    /// A non-auth collection has no accounts to overwrite.
    #[test]
    fn a_non_auth_document_is_never_an_overwritten_account() {
        let def = CollectionDefinition::new("posts");
        let locale = LocaleConfig::default();
        let target = ImportTarget::builder("posts", &def, &locale).build();

        // No table exists: a non-auth collection is never queried.
        let conn = InMemoryConn::open();
        assert!(!overwrites_account(&conn, &target, "p1").unwrap());
    }

    /// An account of a collection without password login logs in some other
    /// way, so it isn't reported as left without a password.
    #[test]
    fn accounts_without_password_login_lack_no_password() {
        let mut auth = Auth::new(true);
        auth.methods.clear();
        let mut def = CollectionDefinition::new("members");
        def.auth = Some(auth);
        let locale = LocaleConfig::default();
        let target = ImportTarget::builder("members", &def, &locale).build();

        // No table exists: a collection without password login is never queried.
        let conn = InMemoryConn::open();
        assert!(!lacks_password(&conn, &target, "m1").unwrap());
    }
}
