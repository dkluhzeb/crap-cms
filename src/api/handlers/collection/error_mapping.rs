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
