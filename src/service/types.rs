//! Shared type definitions for the service layer.

use std::{collections::HashMap, fmt};

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{Document, validate::ValidationError},
    db::LocaleContext,
};

/// Typed service-layer errors that callers can match on for surface-specific handling.
#[derive(Debug)]
pub enum ServiceError {
    /// Collection-level access denied (read, create, update, delete, trash).
    AccessDenied(String),
    /// Document not found.
    NotFound(String),
    /// Ref count protection: document is referenced by others.
    Referenced { id: String, count: i64 },
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
            Self::AccessDenied(msg) => write!(f, "{msg}"),
            Self::NotFound(msg) => write!(f, "{msg}"),
            Self::Referenced { id, count } => {
                write!(f, "Cannot delete '{id}': referenced by {count} document(s)")
            }
            Self::Validation(ve) => write!(f, "{ve}"),
            Self::HookError(msg) => write!(f, "{msg}"),
            Self::UniqueViolation(field) => {
                write!(f, "Unique constraint violated for field '{field}'")
            }
            Self::AccountLocked => write!(f, "Account is locked"),
            Self::EmailNotVerified => write!(f, "Email not verified"),
            Self::InvalidCredentials => write!(f, "Invalid credentials"),
            Self::InvalidToken { kind, reason } => {
                write!(f, "Invalid {kind} token: {reason}")
            }
            Self::Transient(e) => write!(f, "{e:#}"),
            Self::Internal(e) => write!(f, "{e:#}"),
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
        // Preserve structured validation errors rather than wrapping as Internal.
        if let Some(ve) = e.downcast_ref::<ValidationError>() {
            return Self::Validation(ve.clone());
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
    /// Checks for known error types (ValidationError) and string patterns
    /// (transient DB errors, hook errors, unique constraint violations).
    /// `db_kind` selects backend-specific patterns (`"sqlite"`, `"postgres"`).
    pub fn classify(e: anyhow::Error, db_kind: &str) -> Self {
        // Structured validation errors — preserve the typed variant.
        if let Some(ve) = e.downcast_ref::<ValidationError>() {
            return Self::Validation(ve.clone());
        }

        let msg = e.to_string();

        // Transient / retryable DB errors.
        let is_transient = msg.contains("Timed out waiting")
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

        // Unique constraint violations — extract the field name.
        if let Some(rest) = msg.strip_prefix("UNIQUE constraint failed: ") {
            let field = rest
                .find('.')
                .map(|pos| rest[pos + 1..].to_string())
                .unwrap_or_else(|| rest.to_string());
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
    pub fn reclassify(self, db_kind: &str) -> Self {
        match self {
            Self::Internal(e) => Self::classify(e, db_kind),
            other => other,
        }
    }

    /// Convert to an anyhow::Error, preserving the original error chain for Internal/Transient.
    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Internal(inner) | Self::Transient(inner) => inner,
            Self::Validation(ve) => anyhow::Error::new(ve),
            other => anyhow::anyhow!("{other}"),
        }
    }

    /// Returns `true` if this is a validation error.
    pub fn is_validation(&self) -> bool {
        matches!(self, Self::Validation(_))
    }

    /// Extract the `ValidationError` if this is a Validation variant.
    pub fn into_validation(self) -> Option<ValidationError> {
        match self {
            Self::Validation(ve) => Some(ve),
            _ => None,
        }
    }
}

use super::{AfterChangeInputBuilder, PersistOptionsBuilder, WriteInputBuilder};

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, HashMap<String, Value>);

/// Input data for a write operation (create/update). Bundles the 6 data parameters
/// that callers provide, reducing argument count on public API functions.
pub struct WriteInput<'a> {
    pub data: HashMap<String, String>,
    pub join_data: &'a HashMap<String, Value>,
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale: Option<String>,
    pub draft: bool,
    pub ui_locale: Option<String>,
}

impl<'a> WriteInput<'a> {
    /// Create a builder with the required data and join_data fields.
    pub fn builder(
        data: HashMap<String, String>,
        join_data: &'a HashMap<String, Value>,
    ) -> WriteInputBuilder<'a> {
        WriteInputBuilder::new(data, join_data)
    }
}

/// Bundled parameters for after-change hook invocation.
pub(crate) struct AfterChangeInput<'a> {
    pub slug: &'a str,
    pub operation: &'a str,
    pub locale: Option<String>,
    pub is_draft: bool,
    pub req_context: HashMap<String, Value>,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
}

impl<'a> AfterChangeInput<'a> {
    /// Create a builder with the required slug and operation.
    pub fn builder(slug: &'a str, operation: &'a str) -> AfterChangeInputBuilder<'a> {
        AfterChangeInputBuilder::new(slug, operation)
    }
}

/// Optional parameters for the persist_create operation.
#[derive(Default)]
pub struct PersistOptions<'a> {
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale_config: Option<&'a LocaleConfig>,
    pub is_draft: bool,
}

impl<'a> PersistOptions<'a> {
    /// Create a builder with all fields defaulted.
    pub fn builder() -> PersistOptionsBuilder<'a> {
        PersistOptionsBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use crate::core::validate::{FieldError, ValidationError};

    use super::*;

    // ── classify ────────────────────────────────────────────────────

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
        assert!(se.is_validation());
    }

    #[test]
    fn from_anyhow_generic_becomes_internal() {
        let e = anyhow!("generic error");
        let se: ServiceError = e.into();
        assert!(matches!(se, ServiceError::Internal(_)));
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
