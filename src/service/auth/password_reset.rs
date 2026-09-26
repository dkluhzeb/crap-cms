//! Resetting a password with an emailed reset token — the one operation every
//! surface (admin form, gRPC) calls, so they share its transaction semantics.

use crate::{
    core::{CollectionDefinition, collection::Auth},
    service::{
        AppInfra, ServiceContext, ServiceError,
        auth::tokens::consume_reset_token,
        commit_admitted,
        helpers::{EmptyPassword, validate_password_policy},
    },
};

/// A password reset request: the token from the reset link and the new
/// password.
pub struct PasswordReset<'a> {
    /// The reset token from the emailed link.
    pub token: &'a str,
    /// The new plaintext password.
    pub password: &'a str,
}

impl<'a> PasswordReset<'a> {
    /// A reset of the password `token` belongs to, to `password`.
    #[must_use]
    pub fn new(token: &'a str, password: &'a str) -> Self {
        Self { token, password }
    }
}

/// The refusal for a token no candidate collection holds.
fn token_not_found() -> ServiceError {
    ServiceError::InvalidToken {
        kind: "reset",
        reason: "not found",
    }
}

/// Whether `def` can hold a reset token: an auth collection with local
/// password login enabled.
fn accepts_password_reset(def: &CollectionDefinition) -> bool {
    def.is_auth_collection() && def.auth.as_ref().is_some_and(Auth::password_login_enabled)
}

/// Tear down the reset user's open live-update streams. Runs POST-COMMIT only:
/// a reset is a privilege-revoking action (it bumps `_session_version`), and an
/// already-connected stream never makes another request to notice.
fn publish_reset(infra: &AppInfra, slug: &str, user_id: &str) {
    ServiceContext::slug_only(slug)
        .invalidation_transport(Some(infra.invalidation_transport.clone()))
        .build()
        .publish_user_invalidation(user_id);
}

/// Consume the token in whichever candidate holds it, in ONE transaction that
/// commits only on success — any failure drops it, rolling back whatever the
/// attempt wrote.
fn reset_in_transaction<'d>(
    infra: &AppInfra,
    candidates: impl IntoIterator<Item = &'d CollectionDefinition>,
    reset: &PasswordReset,
) -> Result<(), ServiceError> {
    let mut conn = infra.pool.write()?;
    // SELECT-then-UPDATE (find the token row, then write the new hash): take
    // the write lock up front. A DEFERRED transaction would risk
    // `SQLITE_BUSY_SNAPSHOT` under concurrent writers.
    let tx = conn.transaction_immediate()?;
    let candidates = candidates
        .into_iter()
        .filter(|def| accepts_password_reset(def));

    for def in candidates {
        let ctx = ServiceContext::collection(&def.slug, def)
            .conn(&tx)
            .locale_config(Some(&infra.locale_config))
            .build();

        match consume_reset_token(&ctx, reset.token, reset.password) {
            Ok(user_id) => {
                commit_admitted(tx)?;
                publish_reset(infra, &def.slug, &user_id);

                return Ok(());
            }
            Err(ServiceError::InvalidToken {
                reason: "not found",
                ..
            }) => {}
            Err(e) => return Err(e),
        }
    }

    Err(token_not_found())
}

/// Reset a password with a reset token, searching `candidates` for the
/// collection that holds it (the admin form searches every collection, gRPC
/// names one). Candidates without local password login are skipped.
///
/// The new password is checked against the configured policy before anything
/// is read. The token lookup and the password write share one transaction,
/// committed under the request's commit gate only on success: a refused,
/// expired, locked-out or failed attempt writes nothing on any surface.
///
/// # Errors
///
/// - [`ServiceError::Validation`] — the password violates the policy.
/// - [`ServiceError::InvalidToken`] — no candidate holds the token (`"not
///   found"`, also for a locked account) or it has expired (`"expired"`).
/// - [`ServiceError::Transient`] — the request's commit deadline passed, or a
///   transient backend error.
/// - A backend error from the pool, the transaction or the write.
pub fn reset_password_with_token<'d>(
    infra: &AppInfra,
    candidates: impl IntoIterator<Item = &'d CollectionDefinition>,
    reset: &PasswordReset,
) -> Result<(), ServiceError> {
    validate_password_policy(
        true,
        Some(reset.password),
        Some(&infra.password_policy),
        EmptyPassword::IsRejected,
    )?;

    reset_in_transaction(infra, candidates, reset).map_err(|e| e.reclassify(infra.pool.kind()))
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::{sync::Arc, time::Instant};

    use chrono::Utc;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        admin::test_support::test_infra_with_events,
        core::{CommitGate, FieldDefinition, FieldType, in_commit_gate},
        db::{
            DbConnection, DbValue,
            query::{self, TokenGrant},
        },
    };

    /// An auth `users` collection with local password login.
    fn users_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::enabled());
        def.fields = vec![
            FieldDefinition::builder("email", FieldType::Email)
                .unique(true)
                .build(),
        ];

        def
    }

    /// A migrated infra with user `u1` holding reset token `tok`, expiring
    /// `ttl` seconds from now.
    fn infra_with_token(ttl: i64) -> (TempDir, Arc<AppInfra>) {
        let (tmp, infra, _rx) = test_infra_with_events(users_def());
        let conn = infra.pool.write().unwrap();

        conn.execute(
            "INSERT INTO users (id, email) VALUES (?1, ?2)",
            &[
                DbValue::Text("u1".to_string()),
                DbValue::Text("u1@example.com".to_string()),
            ],
        )
        .unwrap();
        query::set_reset_token(
            &conn,
            &TokenGrant::builder("users", "u1", "tok", Utc::now().timestamp() + ttl).build(),
        )
        .unwrap();

        drop(conn);

        (tmp, infra)
    }

    /// Whether `tok` is still stored (expired or not).
    fn token_stored(infra: &AppInfra) -> bool {
        let conn = infra.pool.get().unwrap();
        let def = users_def();

        query::find_by_reset_token(&conn, &def, "tok", None)
            .unwrap()
            .is_some()
    }

    fn reset(infra: &AppInfra, token: &str, password: &str) -> Result<(), ServiceError> {
        let def = users_def();

        reset_password_with_token(infra, [&def], &PasswordReset::new(token, password))
    }

    #[test]
    fn a_valid_token_resets_the_password_and_is_consumed() {
        let (_tmp, infra) = infra_with_token(600);

        reset(&infra, "tok", "newpass123").expect("reset succeeds");

        assert!(!token_stored(&infra), "a used reset token must be gone");
        let conn = infra.pool.get().unwrap();
        assert!(query::has_password(&conn, "users", "u1").unwrap());
    }

    #[test]
    fn an_unknown_token_is_not_found() {
        let (_tmp, infra) = infra_with_token(600);

        let err = reset(&infra, "nope", "newpass123").unwrap_err();

        assert!(matches!(
            err,
            ServiceError::InvalidToken {
                reason: "not found",
                ..
            }
        ));
        assert!(token_stored(&infra));
    }

    /// Regression: the admin surface committed whatever a refused attempt had
    /// written while gRPC rolled it back. A reset whose commit the request's
    /// gate refuses leaves the password and the token exactly as they were.
    #[test]
    fn a_refused_commit_writes_nothing() {
        let (_tmp, infra) = infra_with_token(600);

        let late = CommitGate::new(Instant::now());
        let refused = in_commit_gate(Some(late), || reset(&infra, "tok", "newpass123"));

        assert!(
            matches!(refused, Err(ServiceError::Transient(_))),
            "{refused:?}"
        );
        assert!(
            token_stored(&infra),
            "the token must survive a refused commit"
        );
        let conn = infra.pool.get().unwrap();
        assert!(!query::has_password(&conn, "users", "u1").unwrap());
    }

    /// An expired token is refused and nothing is written: the token row is
    /// left as it was.
    #[test]
    fn an_expired_token_is_refused_and_nothing_is_written() {
        let (_tmp, infra) = infra_with_token(-10);

        let err = reset(&infra, "tok", "newpass123").unwrap_err();

        assert!(matches!(
            err,
            ServiceError::InvalidToken {
                reason: "expired",
                ..
            }
        ));
        assert!(token_stored(&infra), "a refused attempt must write nothing");
        let conn = infra.pool.get().unwrap();
        assert!(!query::has_password(&conn, "users", "u1").unwrap());
    }

    #[test]
    fn a_policy_violation_is_refused_before_the_token_is_touched() {
        let (_tmp, infra) = infra_with_token(600);

        let err = reset(&infra, "tok", "ab").unwrap_err();

        assert!(matches!(err, ServiceError::Validation(_)), "{err:?}");
        assert!(
            token_stored(&infra),
            "the token must survive a policy refusal"
        );
    }

    /// A collection without local password login never matches a token.
    #[test]
    fn a_collection_without_password_login_is_skipped() {
        let (_tmp, infra) = infra_with_token(600);
        let mut def = users_def();
        def.auth = None;

        let err =
            reset_password_with_token(&infra, [&def], &PasswordReset::new("tok", "newpass123"))
                .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidToken { .. }));
        assert!(token_stored(&infra));
    }
}
