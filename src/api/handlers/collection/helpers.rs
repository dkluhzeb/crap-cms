//! Shared helpers for collection gRPC handlers.

use std::collections::HashMap;

use tonic::Status;
use tracing::{error, warn};

use crate::{api::content, config::PasswordPolicy};

/// Extract and validate password from an auth collection's data map.
///
/// - If not an auth collection, returns `Ok(None)` (password field stays in data).
/// - If auth collection, removes `"password"` from `data` and validates it.
/// - `allow_empty`: when `true` (update path), an empty password means "no change" -> `Ok(None)`.
///   When `false` (create path), a present password is always validated.
pub(in crate::api::handlers) fn extract_auth_password(
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
pub(in crate::api::handlers) fn strip_denied_proto_fields(
    doc: &mut content::Document,
    denied: &[String],
) {
    if let Some(ref mut s) = doc.fields {
        for name in denied {
            s.fields.remove(name);
        }
    }
}

/// Strip field-level read-denied fields from proto documents.
///
/// Opens a transaction for the Lua access check, then strips denied fields.
/// Used by read and write handlers that return documents to the caller.
pub(in crate::api::handlers) fn strip_read_denied_proto_fields(
    proto_docs: &mut [content::Document],
    conn: &mut crate::db::BoxedConnection,
    runner: &crate::hooks::HookRunner,
    fields: &[crate::core::FieldDefinition],
    user_doc: Option<&crate::core::Document>,
) {
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(e) => {
            error!("Field access check tx error: {}", e);
            return;
        }
    };

    let denied = runner.check_field_read_access(fields, user_doc, &tx);

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    for doc in proto_docs {
        strip_denied_proto_fields(doc, &denied);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::{Struct, Value, value::Kind};

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
