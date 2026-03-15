//! Error mapping for collection gRPC handlers.

use tonic::Status;

/// Map database/task errors to appropriate gRPC status codes.
/// Returns `Status::unavailable` for transient busy/locked/pool timeout errors
/// (enabling client retry), `Status::invalid_argument` for hook/validation errors,
/// `Status::internal` for everything else.
///
/// `db_kind` selects backend-specific error patterns (`"sqlite"`, `"postgres"`).
pub(super) fn map_db_error(e: anyhow::Error, prefix: &str, db_kind: &str) -> Status {
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

    use anyhow::anyhow;

    // ── SQLite patterns ─────────────────────────────────────────────

    #[test]
    fn map_db_error_transient_locked_sqlite() {
        let e = anyhow!("database is locked");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_busy_sqlite() {
        let e = anyhow!("SQLITE_BUSY error");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_pool() {
        let e = anyhow!("Timed out waiting for connection pool");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_hook_error_sqlite() {
        let e = anyhow!("hook error: title is required");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("hook error:"));
    }

    #[test]
    fn map_db_error_validation_error() {
        let e = anyhow!("Validation failed: email invalid");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_unique_constraint_sqlite() {
        let e = anyhow!("UNIQUE constraint failed: posts.slug");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_unknown() {
        let e = anyhow!("something unexpected happened");
        let status = map_db_error(e, "test", "sqlite");
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), "Internal error");
    }

    // ── Postgres patterns ───────────────────────────────────────────

    #[test]
    fn map_db_error_transient_connection_refused_postgres() {
        let e = anyhow!("connection refused");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_too_many_clients_postgres() {
        let e = anyhow!("too many clients already");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_transient_reserved_slots_postgres() {
        let e = anyhow!("remaining connection slots are reserved");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_duplicate_key_postgres() {
        let e = anyhow!("duplicate key value violates unique constraint");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_foreign_key_postgres() {
        let e = anyhow!("violates foreign key constraint");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn map_db_error_pool_timeout_postgres() {
        let e = anyhow!("Timed out waiting for connection pool");
        let status = map_db_error(e, "test", "postgres");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    // ── Unknown backend — only generic patterns match ───────────────

    #[test]
    fn map_db_error_unknown_backend_pool_timeout() {
        let e = anyhow!("Timed out waiting for connection pool");
        let status = map_db_error(e, "test", "unknown");
        assert_eq!(status.code(), tonic::Code::Unavailable);
    }

    #[test]
    fn map_db_error_unknown_backend_sqlite_pattern_not_matched() {
        let e = anyhow!("SQLITE_BUSY error");
        let status = map_db_error(e, "test", "unknown");
        // Backend-specific pattern should NOT match for unknown backend
        assert_eq!(status.code(), tonic::Code::Internal);
    }
}
