//! Error mapping: ServiceError → tonic::Status, and database error mapping.

use anyhow::Error as AnyhowError;
use tonic::Status;
use tracing::{error, warn};

use crate::service::ServiceError;

impl From<ServiceError> for Status {
    fn from(e: ServiceError) -> Self {
        match e {
            ServiceError::AccessDenied(msg) => Status::permission_denied(msg),
            ServiceError::NotFound(msg) => Status::not_found(msg),
            ServiceError::Referenced { id, count } => Status::failed_precondition(format!(
                "Cannot delete '{id}': referenced by {count} document(s)"
            )),
            ServiceError::Validation(ve) => Status::invalid_argument(ve.to_string()),
            ServiceError::HookError(msg) => Status::invalid_argument(msg),
            ServiceError::UniqueViolation(field) => {
                if field.is_empty() {
                    Status::invalid_argument("Unique constraint violated")
                } else {
                    Status::invalid_argument(format!(
                        "Unique constraint violated for field '{field}'"
                    ))
                }
            }
            ServiceError::AccountLocked => Status::permission_denied("Account is locked"),
            ServiceError::EmailNotVerified => Status::permission_denied("Email not verified"),
            ServiceError::InvalidCredentials => Status::unauthenticated("Invalid credentials"),
            ServiceError::InvalidToken { kind, reason } => {
                Status::invalid_argument(format!("Invalid {kind} token: {reason}"))
            }
            ServiceError::Transient(e) => {
                warn!("Transient error: {}", e);
                Status::unavailable("Service temporarily unavailable, please retry")
            }
            ServiceError::Internal(e) => {
                error!("Internal error: {}", e);
                Status::internal("Internal error")
            }
        }
    }
}

/// Map database/task errors to appropriate gRPC status codes.
/// Returns `Status::unavailable` for transient busy/locked/pool timeout errors
/// (enabling client retry), `Status::invalid_argument` for hook/validation errors,
/// `Status::internal` for everything else.
///
/// `db_kind` selects backend-specific error patterns (`"sqlite"`, `"postgres"`).
pub(in crate::api::handlers::collection) fn map_db_error(
    e: AnyhowError,
    prefix: &str,
    db_kind: &str,
) -> Status {
    let msg = e.to_string();
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

    // Hook/validation errors are user-facing — pass the message through.
    let is_hook_error = msg.contains("hook error:")
        || msg.contains("validation error:")
        || msg.contains("Validation failed:")
        || msg.contains("runtime error:")
        || match db_kind {
            "sqlite" => msg.contains("UNIQUE constraint failed"),
            "postgres" => {
                msg.contains("duplicate key value violates unique constraint")
                    || msg.contains("violates foreign key constraint")
            }
            _ => false,
        };

    if is_transient {
        warn!("{}: {}", prefix, msg);

        Status::unavailable("Service temporarily unavailable, please retry")
    } else if is_hook_error {
        warn!("{}: {}", prefix, msg);

        Status::invalid_argument(sanitize_constraint_error(&msg))
    } else {
        error!("{}: {}", prefix, msg);

        Status::internal("Internal error")
    }
}

/// Sanitize database constraint error messages to avoid leaking internal schema details.
///
/// Converts "UNIQUE constraint failed: table.column" to a user-friendly message
/// that only exposes the column name. Non-constraint messages are returned unchanged.
fn sanitize_constraint_error(msg: &str) -> String {
    // SQLite: "UNIQUE constraint failed: table.column"
    if let Some(rest) = msg.strip_prefix("UNIQUE constraint failed: ")
        && let Some(dot_pos) = rest.find('.')
    {
        let column = &rest[dot_pos + 1..];

        return format!("Unique constraint violated for field '{}'", column);
    }

    // PostgreSQL: "duplicate key value violates unique constraint" — already generic enough,
    // but we could sanitize further if needed. For now, pass through.
    msg.to_string()
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use tonic::Code;

    use super::*;

    // ── SQLite patterns ─────────────────────────────────────────────

    #[test]
    fn map_db_error_transient_locked_sqlite() {
        let e = anyhow!("database is locked");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_busy_sqlite() {
        let e = anyhow!("SQLITE_BUSY error");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_pool() {
        let e = anyhow!("Timed out waiting for connection pool");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn map_db_error_hook_error_sqlite() {
        let e = anyhow!("hook error: title is required");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(status.message().contains("hook error:"));
    }

    #[test]
    fn map_db_error_validation_error() {
        let e = anyhow!("Validation failed: email invalid");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_unique_constraint_sqlite() {
        let e = anyhow!("UNIQUE constraint failed: posts.slug");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::InvalidArgument);
        assert_eq!(
            status.message(),
            "Unique constraint violated for field 'slug'",
            "should sanitize table.column to just column name"
        );
    }

    #[test]
    fn map_db_error_unknown() {
        let e = anyhow!("something unexpected happened");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::Internal);
        assert_eq!(status.message(), "Internal error");
    }

    // ── Postgres patterns ───────────────────────────────────────────

    #[test]
    fn map_db_error_transient_connection_refused_postgres() {
        let e = anyhow!("connection refused");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_too_many_clients_postgres() {
        let e = anyhow!("too many clients already");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_reserved_slots_postgres() {
        let e = anyhow!("remaining connection slots are reserved");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn map_db_error_duplicate_key_postgres() {
        let e = anyhow!("duplicate key value violates unique constraint");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_foreign_key_postgres() {
        let e = anyhow!("violates foreign key constraint");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_pool_timeout_postgres() {
        let e = anyhow!("Timed out waiting for connection pool");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), Code::Unavailable);
    }

    // ── Unknown backend — only generic patterns match ───────────────

    #[test]
    fn map_db_error_unknown_backend_pool_timeout() {
        let e = anyhow!("Timed out waiting for connection pool");
        let status = map_db_error(e, "test", "unknown");
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn map_db_error_unknown_backend_sqlite_pattern_not_matched() {
        let e = anyhow!("SQLITE_BUSY error");
        let status = map_db_error(e, "test", "unknown");
        // Backend-specific pattern should NOT match for unknown backend
        assert_eq!(status.code(), Code::Internal);
    }

    // ── sanitize_constraint_error tests ──────────────────────────────

    #[test]
    fn sanitize_unique_constraint_extracts_column() {
        let msg = "UNIQUE constraint failed: users.email";
        assert_eq!(
            sanitize_constraint_error(msg),
            "Unique constraint violated for field 'email'"
        );
    }

    #[test]
    fn sanitize_unique_constraint_different_table() {
        let msg = "UNIQUE constraint failed: posts.slug";
        assert_eq!(
            sanitize_constraint_error(msg),
            "Unique constraint violated for field 'slug'"
        );
    }

    #[test]
    fn sanitize_non_constraint_message_unchanged() {
        let msg = "hook error: title is required";
        assert_eq!(sanitize_constraint_error(msg), msg);
    }

    #[test]
    fn sanitize_postgres_duplicate_key_unchanged() {
        let msg = "duplicate key value violates unique constraint";
        assert_eq!(sanitize_constraint_error(msg), msg);
    }

    /// Regression: unique constraint errors leaked internal table.column names to clients.
    #[test]
    fn map_db_error_unique_constraint_sanitized_message() {
        let e = anyhow!("UNIQUE constraint failed: users.email");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(
            !status.message().contains("users.email"),
            "should not leak table.column"
        );
        assert!(
            status.message().contains("email"),
            "should contain the field name"
        );
    }

    // ── ServiceError -> Status conversion ───────────────────────────

    #[test]
    fn service_error_access_denied_to_permission_denied() {
        let se = ServiceError::AccessDenied("Read access denied".into());
        let status = Status::from(se);
        assert_eq!(status.code(), Code::PermissionDenied);
        assert_eq!(status.message(), "Read access denied");
    }

    #[test]
    fn service_error_not_found_to_not_found() {
        let se = ServiceError::NotFound("Document not found".into());
        let status = Status::from(se);
        assert_eq!(status.code(), Code::NotFound);
    }

    #[test]
    fn service_error_referenced_to_failed_precondition() {
        let se = ServiceError::Referenced {
            id: "abc".into(),
            count: 5,
        };
        let status = Status::from(se);
        assert_eq!(status.code(), Code::FailedPrecondition);
        assert!(status.message().contains("referenced by 5"));
    }

    #[test]
    fn service_error_validation_to_invalid_argument() {
        use crate::core::validate::{FieldError, ValidationError};
        let ve = ValidationError::new(vec![FieldError::new("title", "required")]);
        let se = ServiceError::Validation(ve);
        let status = Status::from(se);
        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(status.message().contains("title"));
    }

    #[test]
    fn service_error_hook_error_to_invalid_argument() {
        let se = ServiceError::HookError("hook error: title is required".into());
        let status = Status::from(se);
        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn service_error_unique_violation_to_invalid_argument() {
        let se = ServiceError::UniqueViolation("email".into());
        let status = Status::from(se);
        assert_eq!(status.code(), Code::InvalidArgument);
        assert!(status.message().contains("email"));
    }

    #[test]
    fn service_error_transient_to_unavailable() {
        let se = ServiceError::Transient(anyhow!("database is locked"));
        let status = Status::from(se);
        assert_eq!(status.code(), Code::Unavailable);
    }

    #[test]
    fn service_error_internal_to_internal() {
        let se = ServiceError::Internal(anyhow!("unexpected"));
        let status = Status::from(se);
        assert_eq!(status.code(), Code::Internal);
        assert_eq!(status.message(), "Internal error");
    }
}
