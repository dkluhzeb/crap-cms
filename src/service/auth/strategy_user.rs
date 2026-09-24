//! Admitting the user a custom auth strategy or an external auth callback
//! names.
//!
//! A strategy hook (per-request or at login) and an OAuth-style callback hook
//! each return a document that only *names* a user. Every such path admits
//! that user through [`admit_strategy_user`], so the account-state rules
//! can't drift between them:
//!
//! - The **stored** row decides: the user must be a stored, non-trashed row of
//!   the collection, not locked, and verified where the collection requires
//!   verification.
//! - The hook's document may only **restrict**: a `_locked` flag it sets
//!   refuses, a `_verified` flag it clears refuses. It can never widen — a
//!   `_verified = true` on the hook's table does not verify an unverified
//!   account, and a missing `_locked` does not unlock a locked one.
//! - The session is built from the stored document, never the hook's table.

use crate::{
    core::{Document, json_truthy},
    service::{
        ServiceContext, ServiceError,
        auth::{get_session_version, is_locked, is_verified, load_user},
    },
};

/// Why the user a strategy named is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyRefusal {
    /// The id names no stored, non-trashed user of the collection.
    NotStored,
    /// The account is locked (stored, or flagged by the hook).
    Locked,
    /// The collection requires verification and the account is unverified
    /// (stored, or flagged by the hook).
    Unverified,
}

impl StrategyRefusal {
    /// Short label for log messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStored => "not stored",
            Self::Locked => "locked",
            Self::Unverified => "unverified",
        }
    }
}

/// Outcome of [`admit_strategy_user`].
#[derive(Debug)]
pub enum StrategyAdmission {
    /// The stored user document and its current session version.
    Admitted {
        user: Document,
        session_version: u64,
    },
    /// The user may not authenticate.
    Refused(StrategyRefusal),
}

/// Admit the user `hook_doc` names into `ctx`'s auth collection, or say why
/// not. `require_verified` is whether the collection requires email
/// verification. See the module docs for the rules.
///
/// `ctx` must be a collection context carrying a `conn` and, for a localized
/// auth collection, the `locale_config` (the stored user is read through
/// [`load_user`]).
///
/// # Errors
///
/// Returns a backend error if any account-state read fails — callers must
/// refuse the user then (fail closed).
pub fn admit_strategy_user(
    ctx: &ServiceContext,
    hook_doc: &Document,
    require_verified: bool,
) -> Result<StrategyAdmission, ServiceError> {
    if let Some(refusal) = account_refusal(ctx, hook_doc, require_verified)? {
        return Ok(StrategyAdmission::Refused(refusal));
    }

    let Some(user) = load_user(ctx, &hook_doc.id)? else {
        return Ok(StrategyAdmission::Refused(StrategyRefusal::NotStored));
    };

    let session_version = get_session_version(ctx, &user.id)?;

    Ok(StrategyAdmission::Admitted {
        user,
        session_version,
    })
}

/// The lock / verification state that refuses the user `hook_doc` names:
/// the stored flags, restricted further (never widened) by the hook's own.
fn account_refusal(
    ctx: &ServiceContext,
    hook_doc: &Document,
    require_verified: bool,
) -> Result<Option<StrategyRefusal>, ServiceError> {
    if hook_sets(hook_doc, "_locked") || is_locked(ctx, &hook_doc.id)? {
        return Ok(Some(StrategyRefusal::Locked));
    }

    if !require_verified {
        return Ok(None);
    }

    if hook_clears(hook_doc, "_verified") || !is_verified(ctx, &hook_doc.id)? {
        return Ok(Some(StrategyRefusal::Unverified));
    }

    Ok(None)
}

/// Whether the hook's document sets the checkbox flag `key` — the shared
/// checkbox rule ([`json_truthy`]): a DB-style integer, a bool, or a stringy
/// `"1"` / `"on"` / `"true"`.
fn hook_sets(doc: &Document, key: &str) -> bool {
    doc.fields.get(key).is_some_and(json_truthy)
}

/// Whether the hook's document explicitly clears the checkbox flag `key`.
/// An absent flag says nothing.
fn hook_clears(doc: &Document, key: &str) -> bool {
    doc.fields.get(key).is_some_and(|v| !json_truthy(v))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{db::InMemoryConn, service::auth::test_support::setup};

    fn hook_doc(fields: &[(&str, Value)]) -> Document {
        let mut doc = Document::builder("u1").build();

        for (key, value) in fields {
            doc.fields.insert((*key).to_string(), value.clone());
        }

        doc
    }

    fn refusal(outcome: &StrategyAdmission) -> Option<StrategyRefusal> {
        match outcome {
            StrategyAdmission::Admitted { .. } => None,
            StrategyAdmission::Refused(refusal) => Some(*refusal),
        }
    }

    /// The hook's flags read through the shared checkbox rule: a DB-style
    /// number, a bool, or a stringy spelling a hook-synthesized table carries.
    #[test]
    fn hook_flags_accept_every_checkbox_shape() {
        for set in [
            json!(1),
            json!(0.5),
            json!(true),
            json!("1"),
            json!("true"),
            json!("on"),
        ] {
            let doc = hook_doc(&[("_locked", set.clone())]);
            assert!(hook_sets(&doc, "_locked"), "{set} sets the flag");
            assert!(!hook_clears(&doc, "_locked"), "{set} does not clear it");
        }

        for cleared in [
            json!(0),
            json!(false),
            json!("0"),
            json!("false"),
            json!(null),
        ] {
            let doc = hook_doc(&[("_verified", cleared.clone())]);
            assert!(
                !hook_sets(&doc, "_verified"),
                "{cleared} does not set the flag"
            );
            assert!(hook_clears(&doc, "_verified"), "{cleared} clears it");
        }

        let absent = hook_doc(&[]);
        assert!(!hook_sets(&absent, "_locked"));
        assert!(
            !hook_clears(&absent, "_verified"),
            "an absent flag says nothing"
        );
    }

    /// A stored, unlocked, verified user is admitted with the stored document.
    #[test]
    fn a_stored_user_is_admitted_from_the_stored_row() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let outcome = admit_strategy_user(&ctx, &hook_doc(&[("email", json!("x@y"))]), true);

        let StrategyAdmission::Admitted { user, .. } = outcome.unwrap() else {
            panic!("expected admission");
        };
        assert_eq!(user.get_str("email"), Some("test@example.com"));
    }

    /// Regression: the per-request path let a hook's `_verified = true`
    /// verify an account the stored row says is unverified.
    #[test]
    fn a_hook_cannot_verify_an_unverified_account() {
        let (conn, def, _) = setup();
        conn.execute("UPDATE users SET _verified = 0 WHERE id = 'u1'", [])
            .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let outcome = admit_strategy_user(&ctx, &hook_doc(&[("_verified", json!(true))]), true);

        assert_eq!(
            refusal(&outcome.unwrap()),
            Some(StrategyRefusal::Unverified)
        );
    }

    /// Regression: the login path ignored a hook's `_locked` flag.
    #[test]
    fn a_hook_can_lock_a_stored_unlocked_account() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let outcome = admit_strategy_user(&ctx, &hook_doc(&[("_locked", json!(1))]), true);

        assert_eq!(refusal(&outcome.unwrap()), Some(StrategyRefusal::Locked));
    }

    /// A hook clearing `_verified` refuses even a stored-verified account.
    #[test]
    fn a_hook_can_unverify_a_verified_account() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let outcome = admit_strategy_user(&ctx, &hook_doc(&[("_verified", json!(0))]), true);

        assert_eq!(
            refusal(&outcome.unwrap()),
            Some(StrategyRefusal::Unverified)
        );
    }

    /// A stored lock refuses whatever the hook's table says.
    #[test]
    fn a_hook_cannot_unlock_a_locked_account() {
        let (conn, def, _) = setup();
        conn.execute("UPDATE users SET _locked = 1 WHERE id = 'u1'", [])
            .unwrap();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let outcome = admit_strategy_user(&ctx, &hook_doc(&[("_locked", json!(false))]), true);

        assert_eq!(refusal(&outcome.unwrap()), Some(StrategyRefusal::Locked));
    }

    /// An id naming no stored user is refused.
    #[test]
    fn an_unknown_id_is_refused() {
        let (conn, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();
        let doc = Document::builder("ghost").build();

        let outcome = admit_strategy_user(&ctx, &doc, false);

        assert_eq!(refusal(&outcome.unwrap()), Some(StrategyRefusal::NotStored));
    }

    /// A trashed user is not stored for authentication.
    #[test]
    fn a_trashed_user_is_refused() {
        let (conn, mut def, _) = setup();
        conn.execute_batch(
            "ALTER TABLE users ADD COLUMN _deleted_at TEXT;
             UPDATE users SET _deleted_at = '2026-01-01T00:00:00Z' WHERE id = 'u1';",
        )
        .unwrap();
        def.soft_delete = true;
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let outcome = admit_strategy_user(&ctx, &hook_doc(&[]), true);

        assert_eq!(refusal(&outcome.unwrap()), Some(StrategyRefusal::NotStored));
    }

    /// A failed account-state read is an error, never an admission.
    #[test]
    fn a_failed_lookup_is_an_error() {
        // No `users` table: every lookup fails.
        let conn = InMemoryConn::open();
        let (_, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        assert!(admit_strategy_user(&ctx, &hook_doc(&[]), true).is_err());
    }

    /// A hook's lock flag refuses before any lookup runs.
    #[test]
    fn a_hook_lock_flag_refuses_without_a_lookup() {
        let conn = InMemoryConn::open();
        let (_, def, _) = setup();
        let ctx = ServiceContext::collection("users", &def)
            .conn(&conn)
            .build();

        let outcome = admit_strategy_user(&ctx, &hook_doc(&[("_locked", json!("on"))]), true);

        assert_eq!(refusal(&outcome.unwrap()), Some(StrategyRefusal::Locked));
    }
}
