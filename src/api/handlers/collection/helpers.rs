//! Shared helpers for collection gRPC handlers.

use std::collections::HashMap;

use crate::config::PasswordPolicy;
use tonic::Status;

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

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_auth_password tests ───────────────────────────────────

    fn default_policy() -> PasswordPolicy {
        PasswordPolicy::default()
    }

    #[test]
    fn password_non_auth_collection_ignored() {
        let mut data = HashMap::from([("password".into(), "secret123".into())]);
        let result = extract_auth_password(&mut data, false, &default_policy(), false).unwrap();
        assert!(result.is_none());
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
}
