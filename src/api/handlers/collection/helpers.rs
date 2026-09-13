//! Shared helpers for collection gRPC handlers.

use tonic::Status;

use crate::core::DocumentFields;

/// Split the password out of an auth collection's data map.
///
/// - Not an auth collection: returns `None` and leaves the field in `data`
///   (a plain collection may have a legitimate `password` field).
/// - Auth collection: removes `"password"` from `data` and returns it.
/// - `allow_empty`: when `true` (update path), an empty password means
///   "no change" → `None`.
///
/// The password POLICY is deliberately not applied here. The service write
/// path is the authoritative chokepoint (`validate_password_policy`, run after
/// the access check on every surface); validating in the codec as well meant
/// an ANONYMOUS caller could tell a policy rejection from an access denial and
/// so read the policy off an unauthenticated endpoint.
///
/// # Errors
///
/// `INVALID_ARGUMENT` when `password` is present but not a string. That is a
/// wire-shape check, not a policy one — it reveals nothing about
/// `[auth.password_policy]`, and without it a number coerced to `""` and
/// created a passwordless account.
pub(in crate::api::handlers) fn extract_auth_password(
    data: &mut DocumentFields,
    is_auth: bool,
    allow_empty: bool,
) -> Result<Option<String>, Status> {
    if !is_auth {
        return Ok(None);
    }

    let Some(value) = data.remove("password") else {
        return Ok(None);
    };

    let Some(pw) = value.as_str() else {
        return Err(Status::invalid_argument("'password' must be a string"));
    };

    if allow_empty && pw.is_empty() {
        return Ok(None);
    }

    Ok(Some(pw.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::*;

    // ── extract_auth_password tests ───────────────────────────────────

    #[test]
    fn password_non_auth_collection_ignored() {
        let mut data: DocumentFields =
            HashMap::from([("password".into(), json!("secret123"))]).into();
        assert!(
            extract_auth_password(&mut data, false, false)
                .unwrap()
                .is_none()
        );
        assert!(data.contains_key("password"));
    }

    #[test]
    fn password_auth_collection_extracted() {
        let mut data: DocumentFields =
            HashMap::from([("password".into(), json!("secret123"))]).into();
        assert_eq!(
            extract_auth_password(&mut data, true, false)
                .unwrap()
                .as_deref(),
            Some("secret123")
        );
        assert!(!data.contains_key("password"));
    }

    #[test]
    fn password_auth_collection_missing() {
        let mut data: DocumentFields = HashMap::from([("title".into(), json!("hello"))]).into();
        assert!(
            extract_auth_password(&mut data, true, false)
                .unwrap()
                .is_none()
        );
    }

    /// The codec no longer judges the password: a weak one is extracted and
    /// handed to the service, which rejects it AFTER the access check — so an
    /// anonymous caller can't read the policy off the endpoint.
    #[test]
    fn password_policy_is_not_applied_in_the_codec() {
        let mut data: DocumentFields = HashMap::from([("password".into(), json!("short"))]).into();
        assert_eq!(
            extract_auth_password(&mut data, true, false)
                .unwrap()
                .as_deref(),
            Some("short")
        );
    }

    /// A non-string password is a wire-shape error, not a silent coercion:
    /// `{"password": 12345}` used to become `""` and create a passwordless
    /// auth document.
    #[test]
    fn password_must_be_a_string() {
        for value in [json!(12345), json!(true), json!(["a"]), json!(null)] {
            let mut data: DocumentFields = HashMap::from([("password".into(), value)]).into();
            let err = extract_auth_password(&mut data, true, false)
                .expect_err("a non-string password must be rejected");
            assert_eq!(err.code(), tonic::Code::InvalidArgument);
        }
    }

    #[test]
    fn password_empty_on_update_returns_none() {
        let mut data: DocumentFields = HashMap::from([("password".into(), json!(""))]).into();
        assert!(
            extract_auth_password(&mut data, true, true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn password_valid_on_update() {
        let mut data: DocumentFields =
            HashMap::from([("password".into(), json!("newsecret123"))]).into();
        assert_eq!(
            extract_auth_password(&mut data, true, true)
                .unwrap()
                .as_deref(),
            Some("newsecret123")
        );
    }
}
