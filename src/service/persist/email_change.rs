//! The email-change step shared by the single and bulk update persist paths.
//!
//! Both paths must react identically to an update that moves a user to a new
//! address: the confirmation the old address carried does not transfer, so the
//! account goes back to unverified and a fresh confirmation mail is queued.

use anyhow::Result;

use crate::{
    core::{CollectionDefinition, Document, DocumentFields, collection::Auth},
    db::{DbConnection, LocaleContext, query},
    service::{ServiceContext, auth},
};

/// Whether an update is about to write an address that differs from the stored
/// one, on a collection that requires email verification.
///
/// Must be called BEFORE anything writes the row — afterwards the stored
/// address is already the new one, including when a draft write-back carries
/// it.
pub(super) fn email_changed(
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    slug: &str,
    id: &str,
    data: &DocumentFields,
    locale_ctx: Option<&LocaleContext>,
) -> bool {
    if !def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
        return false;
    }

    let Some(new_email) = data.get("email").and_then(|v| v.as_str()) else {
        return false;
    };

    let Ok(Some(current)) = query::find_by_id_raw(conn, slug, def, id, locale_ctx, false) else {
        return false;
    };

    current.get_str("email") != Some(new_email)
}

/// Apply the consequences of an address change detected by [`email_changed`]:
/// clear `_verified` and queue a fresh confirmation mail.
///
/// Unverifying goes through the service-level `mark_unverified`, which also
/// bumps `_session_version` and publishes a user invalidation. The raw query
/// only clears the flag, which would leave a session minted for the old
/// address usable — exactly the case (a hijacker swapping the address) the
/// reset is there to remediate.
///
/// # Errors
///
/// Returns a backend error when the unverify write, the verification token, or
/// the queued mail fails — rolling the address change back with them.
pub(super) fn apply_email_change(
    ctx: &ServiceContext,
    doc: &Document,
    changed: bool,
) -> Result<()> {
    if !changed {
        return Ok(());
    }

    auth::mark_unverified(ctx, &doc.id)?;

    // On this context's connection like a create's: the token and the queued
    // mail land with the address change or not at all.
    ctx.maybe_send_verification(doc)?;

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use crate::{
        core::{
            CollectionDefinition, DocumentFields, FieldDefinition, FieldType, collection::Auth,
        },
        service::{PersistOptions, ServiceContext, persist_bulk_update, persist_update},
    };

    fn setup() -> (Connection, CollectionDefinition) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                _revision INTEGER NOT NULL DEFAULT 0,
                email TEXT,
                name TEXT,
                _password_hash TEXT,
                _locked INTEGER DEFAULT 0,
                _verified INTEGER DEFAULT 0,
                _session_version INTEGER DEFAULT 0,
                _reset_token TEXT,
                _reset_token_exp INTEGER,
                _verification_token TEXT,
                _verification_token_exp INTEGER,
                _ref_count INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, email, name, _verified, _session_version)
             VALUES ('u1', 'old@example.com', 'Old', 1, 3)",
            [],
        )
        .unwrap();

        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email).build(),
            FieldDefinition::builder("name", FieldType::Text).build(),
        ];

        (conn, def)
    }

    fn state(conn: &Connection) -> (i64, i64) {
        conn.query_row(
            "SELECT _verified, _session_version FROM users WHERE id = 'u1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    fn fields(pairs: &[(&str, &str)]) -> DocumentFields {
        let mut data = DocumentFields::new();
        for (k, v) in pairs {
            data.insert((*k).to_string(), json!(v));
        }
        data
    }

    /// A bulk update that moves the account to a new address must unverify it
    /// and invalidate its sessions, exactly like the single-document path —
    /// otherwise `UpdateMany` is a way to park an unconfirmed address on a
    /// still-verified account.
    #[test]
    fn bulk_update_email_change_unverifies_and_bumps_session_version() {
        let (conn, def) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        persist_bulk_update(
            &ctx,
            "u1",
            &fields(&[("email", "new@example.com")]),
            &PersistOptions::default(),
        )
        .unwrap();

        let (verified, session_version) = state(&conn);
        assert_eq!(verified, 0, "bulk email change must clear _verified");
        assert!(
            session_version > 3,
            "bulk email change must bump _session_version (still {session_version})"
        );
    }

    /// The single-document path must bump `_session_version` too: clearing
    /// `_verified` alone leaves a token minted for the old address usable.
    #[test]
    fn update_email_change_unverifies_and_bumps_session_version() {
        let (conn, def) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        persist_update(
            &ctx,
            "u1",
            &fields(&[("email", "new@example.com"), ("name", "New")]),
            &PersistOptions::default(),
        )
        .unwrap();

        let (verified, session_version) = state(&conn);
        assert_eq!(verified, 0, "email change must clear _verified");
        assert!(
            session_version > 3,
            "email change must bump _session_version (still {session_version})"
        );
    }

    /// An update that leaves the address alone changes neither flag — on
    /// either path.
    #[test]
    fn update_without_email_change_keeps_verification() {
        let (conn, def) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        persist_update(
            &ctx,
            "u1",
            &fields(&[("email", "old@example.com"), ("name", "Same address")]),
            &PersistOptions::default(),
        )
        .unwrap();
        assert_eq!(state(&conn), (1, 3));

        persist_bulk_update(
            &ctx,
            "u1",
            &fields(&[("name", "No address at all")]),
            &PersistOptions::default(),
        )
        .unwrap();
        assert_eq!(state(&conn), (1, 3));
    }
}
