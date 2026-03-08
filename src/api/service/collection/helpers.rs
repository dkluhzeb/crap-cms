//! Error mapping for collection gRPC handlers.

use tonic::Status;

/// Map database/task errors to appropriate gRPC status codes.
/// Returns `Status::unavailable` for transient SQLite busy/locked/pool timeout errors
/// (enabling client retry), `Status::invalid_argument` for hook/validation errors,
/// `Status::internal` for everything else.
pub(super) fn map_db_error(e: anyhow::Error, prefix: &str) -> Status {
    let msg = e.to_string();
    let is_transient = msg.contains("database is locked")
        || msg.contains("database is busy")
        || msg.contains("SQLITE_BUSY")
        || msg.contains("SQLITE_LOCKED")
        || msg.contains("Timed out waiting")
        || msg.contains("connection pool");
    // Hook/validation errors are user-facing — pass the message through.
    let is_hook_error = msg.contains("hook error:")
        || msg.contains("validation error:")
        || msg.contains("Validation failed:")
        || msg.contains("runtime error:")
        || msg.contains("UNIQUE constraint failed");
    if is_transient {
        tracing::warn!("{}: {}", prefix, msg);
        Status::unavailable("Service temporarily unavailable, please retry")
    } else if is_hook_error {
        tracing::warn!("{}: {}", prefix, msg);
        Status::invalid_argument(msg)
    } else {
        tracing::error!("{}: {}", prefix, msg);
        Status::internal("Internal error")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_db_error_transient_locked() {
        let e = anyhow::anyhow!("database is locked");
        let status = map_db_error(e, "test");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_busy() {
        let e = anyhow::anyhow!("SQLITE_BUSY error");
        let status = map_db_error(e, "test");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_pool() {
        let e = anyhow::anyhow!("Timed out waiting for connection pool");
        let status = map_db_error(e, "test");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_hook_error() {
        let e = anyhow::anyhow!("hook error: title is required");
        let status = map_db_error(e, "test");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("hook error:"));
    }

    #[test]
    fn map_db_error_validation_error() {
        let e = anyhow::anyhow!("Validation failed: email invalid");
        let status = map_db_error(e, "test");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_unique_constraint() {
        let e = anyhow::anyhow!("UNIQUE constraint failed: posts.slug");
        let status = map_db_error(e, "test");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_unknown() {
        let e = anyhow::anyhow!("something unexpected happened");
        let status = map_db_error(e, "test");
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "Internal error");
    }
}
