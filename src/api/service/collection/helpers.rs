//! Shared helpers for collection gRPC handlers.

use std::collections::HashMap;

use tonic::Status;

use crate::{api::content, config::PasswordPolicy};

/// Map database/task errors to appropriate gRPC status codes.
/// Returns `Status::unavailable` for transient busy/locked/pool timeout errors
/// (enabling client retry), `Status::invalid_argument` for hook/validation errors,
/// `Status::internal` for everything else.
///
/// `db_kind` selects backend-specific error patterns (`"sqlite"`, `"postgres"`).
pub(in crate::api::service::collection) fn map_db_error(
    e: anyhow::Error,
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

/// Extract and validate password from an auth collection's data map.
///
/// - If not an auth collection, returns `Ok(None)` (password field stays in data).
/// - If auth collection, removes `"password"` from `data` and validates it.
/// - `allow_empty`: when `true` (update path), an empty password means "no change" → `Ok(None)`.
///   When `false` (create path), a present password is always validated.
pub(in crate::api::service) fn extract_auth_password(
    data: &mut HashMap<String, String>,
    is_auth: bool,
    policy: &PasswordPolicy,
    allow_empty: bool,
) -> Result<Option<String>, Status> {
    if !is_auth {
        return Ok(None);
    }
    let password = data.remove("password");
    let Some(pw) = password else {
        return Ok(None);
    };
    if allow_empty && pw.is_empty() {
        return Ok(None);
    }
    policy
        .validate(&pw)
        .map_err(|e| Status::invalid_argument(e.to_string()))?;
    Ok(Some(pw))
}

/// Strip denied field names from a proto Document's fields map.
pub(in crate::api::service) fn strip_denied_proto_fields(
    doc: &mut content::Document,
    denied: &[String],
) {
    if let Some(ref mut s) = doc.fields {
        for name in denied {
            s.fields.remove(name);
        }
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

    // ── extract_auth_password tests ───────────────────────────────────

    fn default_policy() -> PasswordPolicy {
        PasswordPolicy::default()
    }

    #[test]
    fn password_non_auth_collection_ignored() {
        let mut data = HashMap::from([("password".into(), "secret123".into())]);
        let result = extract_auth_password(&mut data, false, &default_policy(), false).unwrap();
        assert!(result.is_none());
        // password should remain in data for non-auth collections
        assert!(data.contains_key("password"));
    }

    #[test]
    fn password_auth_collection_extracted() {
        let mut data = HashMap::from([("password".into(), "secret123".into())]);
        let result = extract_auth_password(&mut data, true, &default_policy(), false).unwrap();
        assert_eq!(result.as_deref(), Some("secret123"));
        assert!(!data.contains_key("password"));
    }

    #[test]
    fn password_auth_collection_missing() {
        let mut data = HashMap::from([("title".into(), "hello".into())]);
        let result = extract_auth_password(&mut data, true, &default_policy(), false).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn password_too_short_rejected() {
        let mut data = HashMap::from([("password".into(), "short".into())]);
        let err = extract_auth_password(&mut data, true, &default_policy(), false).unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn password_empty_on_update_returns_none() {
        let mut data = HashMap::from([("password".into(), String::new())]);
        let result = extract_auth_password(&mut data, true, &default_policy(), true).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn password_valid_on_update() {
        let mut data = HashMap::from([("password".into(), "newsecret123".into())]);
        let result = extract_auth_password(&mut data, true, &default_policy(), true).unwrap();
        assert_eq!(result.as_deref(), Some("newsecret123"));
    }

    // ── strip_denied_proto_fields tests ───────────────────────────────

    #[test]
    fn strip_denied_fields_removes_specified() {
        use prost_types::{Struct, Value, value::Kind};

        let mut doc = content::Document {
            id: "doc-1".into(),
            collection: "posts".into(),
            fields: Some(Struct {
                fields: [
                    (
                        "title".into(),
                        Value {
                            kind: Some(Kind::StringValue("Hello".into())),
                        },
                    ),
                    (
                        "secret".into(),
                        Value {
                            kind: Some(Kind::StringValue("hidden".into())),
                        },
                    ),
                    (
                        "body".into(),
                        Value {
                            kind: Some(Kind::StringValue("content".into())),
                        },
                    ),
                ]
                .into_iter()
                .collect(),
            }),
            created_at: None,
            updated_at: None,
        };
        strip_denied_proto_fields(&mut doc, &["secret".to_string()]);
        let fields = doc.fields.as_ref().unwrap();
        assert!(fields.fields.contains_key("title"));
        assert!(fields.fields.contains_key("body"));
        assert!(!fields.fields.contains_key("secret"));
    }

    #[test]
    fn strip_denied_fields_no_fields() {
        let mut doc = content::Document {
            id: "doc-1".into(),
            collection: "posts".into(),
            fields: None,
            created_at: None,
            updated_at: None,
        };
        // Should not panic on None fields
        strip_denied_proto_fields(&mut doc, &["anything".to_string()]);
        assert!(doc.fields.is_none());
    }
}
