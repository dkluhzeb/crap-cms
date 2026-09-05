//! Typed service-layer error enum with classification and surface-specific conversion.

use std::fmt;

use anyhow::anyhow;
use tracing::error;

use crate::core::ValidationError;

/// Typed service-layer errors that callers can match on for surface-specific handling.
#[derive(Debug)]
pub enum ServiceError {
    /// Collection-level access denied (read, create, update, delete, trash).
    AccessDenied(String),
    /// Document not found.
    NotFound(String),
    /// Ref count protection: document is referenced by others.
    Referenced { id: String, count: i64 },
    /// A bulk operation matched more documents than the configured limit
    /// (`server.bulk_max_documents`). Nothing was changed.
    LimitExceeded(String),
    /// Structured per-field validation errors (required, unique, custom Lua validators).
    Validation(ValidationError),
    /// Hook execution error with a user-facing message.
    HookError(String),
    /// Unique constraint violation with the offending field name.
    UniqueViolation(String),
    /// Account is locked — authentication or token consumption denied.
    AccountLocked,
    /// Email not verified — login denied.
    EmailNotVerified,
    /// Invalid credentials (email not found or password mismatch).
    InvalidCredentials,
    /// Invalid or expired token (reset or verification).
    InvalidToken {
        kind: &'static str,
        reason: &'static str,
    },
    /// Transient DB error (locked, busy, pool timeout) — caller should retry.
    Transient(anyhow::Error),
    /// Any other internal error.
    Internal(anyhow::Error),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessDenied(msg)
            | Self::NotFound(msg)
            | Self::HookError(msg)
            | Self::LimitExceeded(msg) => {
                write!(f, "{msg}")
            }
            Self::Referenced { id, count } => {
                write!(f, "Cannot delete '{id}': referenced by {count} document(s)")
            }
            Self::Validation(ve) => write!(f, "{ve}"),
            Self::UniqueViolation(field) => {
                write!(f, "Unique constraint violated for field '{field}'")
            }
            Self::AccountLocked => write!(f, "Account is locked"),
            Self::EmailNotVerified => write!(f, "Email not verified"),
            Self::InvalidCredentials => write!(f, "Invalid credentials"),
            Self::InvalidToken { kind, reason } => {
                write!(f, "Invalid {kind} token: {reason}")
            }
            Self::Transient(e) | Self::Internal(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for ServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(e) | Self::Transient(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for ServiceError {
    fn from(e: anyhow::Error) -> Self {
        // A `ServiceError` raised through a layer that speaks `anyhow` (e.g. the
        // hooks layer's `check_access`) round-trips back to its typed self —
        // so a `HookError` from access-constraint validation maps to
        // `invalid_argument`, not `Internal` (which clients retry on).
        let e = match e.downcast::<ServiceError>() {
            Ok(se) => return se,
            Err(e) => e,
        };

        // Preserve structured validation errors rather than wrapping as Internal.
        if let Some(ve) = e.downcast_ref::<ValidationError>() {
            return Self::Validation(ve.clone());
        }
        // Preserve typed `DocumentNotFound` raised by `query::update` /
        // `query::update_partial` when the UPDATE matched zero rows.
        // Without this branch, Update / UpdateMany on a missing id would
        // come back as `Internal` over gRPC — which production clients
        // retry on, masking the underlying bug.
        if let Some(dnf) = e.downcast_ref::<crate::db::query::DocumentNotFound>() {
            return Self::NotFound(dnf.to_string());
        }
        Self::Internal(e)
    }
}

impl From<ValidationError> for ServiceError {
    fn from(ve: ValidationError) -> Self {
        Self::Validation(ve)
    }
}

impl ServiceError {
    /// Classify an anyhow error into the appropriate `ServiceError` variant.
    ///
    /// Checks for known error types (`ValidationError`) and string patterns
    /// (transient DB errors, hook errors, unique constraint violations).
    /// `db_kind` selects backend-specific patterns (`"sqlite"`, `"postgres"`).
    #[must_use]
    pub fn classify(e: anyhow::Error, db_kind: &str) -> Self {
        const UNIQUE_PREFIX: &str = "UNIQUE constraint failed: ";

        // Structured validation errors — preserve the typed variant.
        if let Some(ve) = e.downcast_ref::<ValidationError>() {
            return Self::Validation(ve.clone());
        }

        // Match against the FULL cause chain (`{:#}`), not just the top
        // message: helpers routinely wrap errors in an anyhow context
        // (`DbPool::get` adds "Failed to get DB connection"), and matching
        // `to_string()` alone made every wrapped transient cause — pool
        // timeout, SQLITE_BUSY — classify as internal (500) instead of
        // transient (unavailable/503) on every surface.
        let msg = format!("{e:#}");

        // Transient / retryable DB errors. Both timeout spellings are needed:
        // r2d2's pool timeout is lowercase ("timed out waiting for
        // connection"), the capitalized form covers other wait-timeout
        // sources.
        let is_transient = msg.contains("Timed out waiting")
            || msg.contains("timed out waiting for connection")
            || msg.contains("connection pool")
            || match db_kind {
                "sqlite" => {
                    msg.contains("database is locked")
                        || msg.contains("database is busy")
                        || msg.contains("SQLITE_BUSY")
                        || msg.contains("SQLITE_LOCKED")
                }
                "postgres" => {
                    msg.contains("connection refused")
                        || msg.contains("too many clients")
                        || msg.contains("remaining connection slots are reserved")
                }
                _ => false,
            };
        if is_transient {
            return Self::Transient(e);
        }

        // Unique constraint violations — extract the field name. `find` (not
        // `strip_prefix`): the SQLite message may sit behind context layers in
        // the `{:#}` chain rather than at the start.
        if let Some(pos) = msg.find(UNIQUE_PREFIX) {
            let rest = &msg[pos + UNIQUE_PREFIX.len()..];
            let field = rest
                .find('.')
                .map_or_else(|| rest.to_string(), |dot| rest[dot + 1..].to_string());
            return Self::UniqueViolation(field);
        }
        if msg.contains("duplicate key value violates unique constraint") {
            return Self::UniqueViolation(String::new());
        }
        if db_kind == "postgres" && msg.contains("violates foreign key constraint") {
            return Self::UniqueViolation(String::new());
        }

        // Hook/runtime errors — user-facing messages.
        if msg.contains("hook error:")
            || msg.contains("validation error:")
            || msg.contains("Validation failed:")
            || msg.contains("runtime error:")
        {
            return Self::HookError(msg);
        }

        Self::Internal(e)
    }

    /// Re-classify an `Internal` error using backend-specific string patterns.
    ///
    /// Non-Internal variants pass through unchanged. This is used at the surface
    /// boundary (gRPC, admin) where the backend kind is known.
    #[must_use]
    pub fn reclassify(self, db_kind: &str) -> Self {
        match self {
            Self::Internal(e) => Self::classify(e, db_kind),
            other => other,
        }
    }

    /// Convert to an `anyhow::Error`, preserving the original error chain for Internal/Transient.
    #[must_use]
    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Internal(inner) | Self::Transient(inner) => inner,
            Self::Validation(ve) => anyhow::Error::new(ve),
            other => anyhow!("{other}"),
        }
    }

    /// Like [`Self::into_anyhow`], but **scrubs** `Internal`/`Transient`
    /// for client-facing surfaces (ledger classes F17/P2): their inner
    /// chains carry raw backend/pool text (DB identifiers, driver
    /// vocabulary) that gRPC and the REST upload surface already hide —
    /// MCP must match. The full chain is logged server-side first.
    #[must_use]
    pub fn into_anyhow_scrubbed(self) -> anyhow::Error {
        match self {
            Self::Internal(inner) => {
                error!("Internal error (scrubbed from client): {inner:#}");
                anyhow!("Internal error")
            }
            Self::Transient(inner) => {
                error!("Transient error (scrubbed from client): {inner:#}");
                anyhow!("Temporarily unavailable, retry")
            }
            other => other.into_anyhow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;
    use crate::core::{FieldError, ValidationError};

    // ── classify ────────────────────────────────────────────────────

    /// Regression: a transient cause hidden behind an anyhow context layer
    /// (the shape `DbPool::get` produces — "Failed to get DB connection:
    /// Timed out waiting…") must still classify as `Transient`. `classify`
    /// used to match only `to_string()` (the outermost context), so every
    /// wrapped pool timeout / `SQLITE_BUSY` landed as `Internal` (500) instead
    /// of `Transient` (unavailable/503) on every surface.
    #[test]
    fn classify_matches_transient_cause_behind_context() {
        use anyhow::Context as _;

        // r2d2's actual (lowercase) pool-timeout wording.
        let e = Err::<(), _>(anyhow!("timed out waiting for connection"))
            .context("Failed to get DB connection")
            .unwrap_err();
        assert!(matches!(
            ServiceError::classify(e, "sqlite"),
            ServiceError::Transient(_)
        ));

        let e = Err::<(), _>(anyhow!("database is locked"))
            .context("Failed to update document x in 'posts'")
            .unwrap_err();
        assert!(matches!(
            ServiceError::classify(e, "sqlite"),
            ServiceError::Transient(_)
        ));
    }

    /// The unique-violation field extraction still works when the `SQLite`
    /// message sits behind a context layer in the `{:#}` chain.
    #[test]
    fn classify_unique_violation_behind_context() {
        use anyhow::Context as _;

        let e = Err::<(), _>(anyhow!("UNIQUE constraint failed: users.email"))
            .context("Failed to create document in 'users'")
            .unwrap_err();
        let ServiceError::UniqueViolation(field) = ServiceError::classify(e, "sqlite") else {
            panic!("expected UniqueViolation");
        };
        assert_eq!(field, "email");
    }

    #[test]
    fn classify_validation_error_preserved() {
        let ve = ValidationError::new(vec![FieldError::new("title", "required")]);
        let e = anyhow::Error::new(ve);
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Validation(_)));
    }

    #[test]
    fn classify_transient_sqlite_locked() {
        let e = anyhow!("database is locked");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_transient_sqlite_busy() {
        let e = anyhow!("SQLITE_BUSY error");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_transient_pool_timeout() {
        let e = anyhow!("Timed out waiting for connection pool");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_transient_postgres_connection_refused() {
        let e = anyhow!("connection refused");
        let se = ServiceError::classify(e, "postgres");
        assert!(matches!(se, ServiceError::Transient(_)));
    }

    #[test]
    fn classify_unique_violation_sqlite() {
        let e = anyhow!("UNIQUE constraint failed: users.email");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::UniqueViolation(ref f) if f == "email"));
    }

    #[test]
    fn classify_unique_violation_postgres() {
        let e = anyhow!("duplicate key value violates unique constraint");
        let se = ServiceError::classify(e, "postgres");
        assert!(matches!(se, ServiceError::UniqueViolation(_)));
    }

    #[test]
    fn classify_hook_error() {
        let e = anyhow!("hook error: title is required");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::HookError(_)));
    }

    #[test]
    fn classify_validation_string() {
        let e = anyhow!("Validation failed: email invalid");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::HookError(_)));
    }

    #[test]
    fn classify_unknown_falls_to_internal() {
        let e = anyhow!("something unexpected");
        let se = ServiceError::classify(e, "sqlite");
        assert!(matches!(se, ServiceError::Internal(_)));
    }

    // ── reclassify ──────────────────────────────────────────────────

    #[test]
    fn reclassify_internal_to_transient() {
        let se = ServiceError::Internal(anyhow!("database is locked"));
        let re = se.reclassify("sqlite");
        assert!(matches!(re, ServiceError::Transient(_)));
    }

    #[test]
    fn reclassify_non_internal_passes_through() {
        let se = ServiceError::AccessDenied("denied".into());
        let re = se.reclassify("sqlite");
        assert!(matches!(re, ServiceError::AccessDenied(_)));
    }

    // ── From<anyhow::Error> ─────────────────────────────────────────

    #[test]
    fn from_anyhow_validation_extracted() {
        let ve = ValidationError::new(vec![FieldError::new("x", "bad")]);
        let e = anyhow::Error::new(ve);
        let se: ServiceError = e.into();
        assert!(matches!(se, ServiceError::Validation(_)));
    }

    #[test]
    fn from_anyhow_generic_becomes_internal() {
        let e = anyhow!("generic error");
        let se: ServiceError = e.into();
        assert!(matches!(se, ServiceError::Internal(_)));
    }

    /// A `ServiceError` raised through the hooks layer (which speaks `anyhow`)
    /// round-trips back to its typed self, so a `HookError` from access-constraint
    /// validation maps to `invalid_argument`, not `Internal`. Regression for the
    /// access-constraint chokepoint surfacing the right gRPC status.
    #[test]
    fn from_anyhow_recovers_typed_service_error() {
        let e = anyhow::Error::new(ServiceError::HookError("bad access constraint".into()));
        let se: ServiceError = e.into();
        match se {
            ServiceError::HookError(msg) => assert_eq!(msg, "bad access constraint"),
            other => panic!("expected HookError, got {other:?}"),
        }
    }

    // ── into_anyhow ─────────────────────────────────────────────────

    #[test]
    fn into_anyhow_preserves_internal() {
        let se = ServiceError::Internal(anyhow!("inner error"));
        let e = se.into_anyhow();
        assert!(e.to_string().contains("inner error"));
    }

    #[test]
    fn into_anyhow_validation_roundtrips() {
        let ve = ValidationError::new(vec![FieldError::new("a", "b")]);
        let se = ServiceError::Validation(ve);
        let e = se.into_anyhow();
        assert!(e.downcast_ref::<ValidationError>().is_some());
    }

    // ── Display ─────────────────────────────────────────────────────

    #[test]
    fn display_referenced() {
        let se = ServiceError::Referenced {
            id: "doc-1".into(),
            count: 3,
        };
        assert_eq!(
            se.to_string(),
            "Cannot delete 'doc-1': referenced by 3 document(s)"
        );
    }

    #[test]
    fn display_unique_violation() {
        let se = ServiceError::UniqueViolation("email".into());
        assert_eq!(
            se.to_string(),
            "Unique constraint violated for field 'email'"
        );
    }
}
