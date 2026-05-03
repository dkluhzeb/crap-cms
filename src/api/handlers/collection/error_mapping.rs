//! Error mapping: ServiceError → tonic::Status.

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
                // Per gRPC spec, conflict-with-existing-resource is
                // `ALREADY_EXISTS` (code 6), not `INVALID_ARGUMENT` (3).
                // Clients use this distinction to drive retry / "use the
                // existing resource" / "ask the user to pick another
                // value" flows.
                if field.is_empty() {
                    Status::already_exists("Unique constraint violated")
                } else {
                    Status::already_exists(format!(
                        "Unique constraint violated for field '{field}'"
                    ))
                }
            }
            ServiceError::AccountLocked => Status::permission_denied("Account is locked"),
            ServiceError::EmailNotVerified => Status::permission_denied("Email not verified"),
            ServiceError::InvalidCredentials => Status::unauthenticated("Invalid credentials"),
            ServiceError::InvalidToken { kind, reason } => {
                // Per gRPC spec, missing/invalid auth credentials map to
                // `UNAUTHENTICATED` (code 16). Client SDKs key token
                // refresh on this code; mapping to `INVALID_ARGUMENT`
                // would mask auth failures as logic errors.
                Status::unauthenticated(format!("Invalid {kind} token: {reason}"))
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

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use tonic::Code;

    use super::*;

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

    /// Regression: was previously mapped to `InvalidArgument`, but per
    /// gRPC spec a unique-constraint conflict is `AlreadyExists` (code 6).
    /// Client SDKs branch on this code for "use existing / pick another"
    /// flows; the old mapping made conflicts indistinguishable from plain
    /// validation errors.
    #[test]
    fn service_error_unique_violation_to_already_exists() {
        let se = ServiceError::UniqueViolation("email".into());
        let status = Status::from(se);
        assert_eq!(status.code(), Code::AlreadyExists);
        assert!(status.message().contains("email"));
    }

    /// Regression: was previously `InvalidArgument`, now correctly
    /// `Unauthenticated`. Client SDKs trigger token-refresh on this code;
    /// the old mapping looked like a malformed request and silently
    /// suppressed refresh.
    #[test]
    fn service_error_invalid_token_to_unauthenticated() {
        let se = ServiceError::InvalidToken {
            kind: "session",
            reason: "expired",
        };
        let status = Status::from(se);
        assert_eq!(status.code(), Code::Unauthenticated);
        assert!(status.message().contains("session"));
        assert!(status.message().contains("expired"));
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
