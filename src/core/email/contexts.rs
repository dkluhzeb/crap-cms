//! Typed contexts for built-in email templates.
//!
//! Each struct mirrors the variables used in the matching `templates/email/*.hbs`
//! file. Constructed at the call site, serialized by `EmailRenderer::render`.

use serde::Serialize;

/// Context for the `password_reset` email template.
#[derive(Serialize)]
pub struct PasswordResetEmailContext<'a> {
    pub reset_url: &'a str,
    pub expiry_minutes: u64,
    pub from_name: &'a str,
}

/// Context for the `verify_email` email template.
#[derive(Serialize)]
pub struct VerifyEmailContext<'a> {
    pub verify_url: &'a str,
    pub from_name: &'a str,
}

/// Context for the `mfa_code` email template.
#[derive(Serialize)]
pub struct MfaCodeEmailContext<'a> {
    pub code: &'a str,
    pub expiry_minutes: u64,
    pub from_name: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // These structs are the contract with `templates/email/*.hbs`. Pinning the
    // serialized key set catches a field rename that would silently break a
    // template variable (`{{reset_url}}` → empty).

    #[test]
    fn password_reset_context_keys() {
        let ctx = PasswordResetEmailContext {
            reset_url: "u",
            expiry_minutes: 30,
            from_name: "n",
        };
        assert_eq!(
            serde_json::to_value(&ctx).unwrap(),
            json!({ "reset_url": "u", "expiry_minutes": 30, "from_name": "n" })
        );
    }

    #[test]
    fn verify_email_context_keys() {
        let ctx = VerifyEmailContext {
            verify_url: "u",
            from_name: "n",
        };
        assert_eq!(
            serde_json::to_value(&ctx).unwrap(),
            json!({ "verify_url": "u", "from_name": "n" })
        );
    }

    #[test]
    fn mfa_code_context_keys() {
        let ctx = MfaCodeEmailContext {
            code: "123",
            expiry_minutes: 5,
            from_name: "n",
        };
        assert_eq!(
            serde_json::to_value(&ctx).unwrap(),
            json!({ "code": "123", "expiry_minutes": 5, "from_name": "n" })
        );
    }
}
