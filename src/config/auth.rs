//! Authentication and password policy configuration.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::parsing::serde_duration;
use crate::core::JwtSecret;

/// JWT authentication settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    /// JWT secret. If empty, a random secret is generated on first startup and
    /// persisted to `data/.jwt_secret`. Set explicitly for multi-instance deployments.
    pub secret: JwtSecret,
    /// Default token expiry in seconds (can be overridden per-collection).
    /// Accepts integer seconds or human-readable string ("2h", "7200").
    #[serde(with = "serde_duration")]
    pub token_expiry: u64,
    /// Max failed login attempts before lockout. Default: 5.
    pub max_login_attempts: u32,
    /// Lockout window in seconds. Default: 300 (5 minutes).
    /// Accepts integer seconds or human-readable string ("5m", "300").
    #[serde(with = "serde_duration")]
    pub login_lockout_seconds: u64,
    /// Password reset token expiry in seconds. Default: 3600 (1 hour).
    /// Accepts integer seconds or human-readable string ("1h", "3600").
    #[serde(with = "serde_duration")]
    pub reset_token_expiry: u64,
    /// Max forgot-password requests per email before rate limiting. Default: 3.
    pub max_forgot_password_attempts: u32,
    /// Forgot-password rate limit window in seconds. Default: 900 (15 minutes).
    /// Accepts integer seconds or human-readable string ("15m", "900").
    #[serde(with = "serde_duration")]
    pub forgot_password_window_seconds: u64,
    /// Max failed login attempts per IP before lockout. Default: 20.
    /// Higher than per-email to tolerate shared IPs (offices, NAT).
    pub max_ip_login_attempts: u32,
    /// Password strength requirements.
    #[serde(default)]
    pub password_policy: PasswordPolicy,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret: JwtSecret::new(""),
            token_expiry: 7200,
            max_login_attempts: 5,
            max_ip_login_attempts: 20,
            login_lockout_seconds: 300,
            reset_token_expiry: 3600,
            max_forgot_password_attempts: 3,
            forgot_password_window_seconds: 900,
            password_policy: PasswordPolicy::default(),
        }
    }
}

/// Password strength requirements. Applied to all password-setting paths:
/// user creation (admin, gRPC, CLI), password reset, and password update.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PasswordPolicy {
    /// Minimum password length. Default: 8.
    pub min_length: usize,
    /// Maximum password length. Default: 128. Prevents DoS via Argon2 on huge inputs.
    pub max_length: usize,
    /// Require at least one uppercase letter (A-Z). Default: false.
    pub require_uppercase: bool,
    /// Require at least one lowercase letter (a-z). Default: false.
    pub require_lowercase: bool,
    /// Require at least one digit (0-9). Default: false.
    pub require_digit: bool,
    /// Require at least one special character (non-alphanumeric). Default: false.
    pub require_special: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: 8,
            max_length: 128,
            require_uppercase: false,
            require_lowercase: false,
            require_digit: false,
            require_special: false,
        }
    }
}

impl PasswordPolicy {
    /// Validate a password against this policy. Returns `Ok(())` if the password
    /// meets all requirements, or `Err` with a human-readable message.
    pub fn validate(&self, password: &str) -> Result<()> {
        if password.len() < self.min_length {
            bail!("Password must be at least {} characters", self.min_length);
        }
        if password.len() > self.max_length {
            bail!("Password must be at most {} characters", self.max_length);
        }
        if self.require_uppercase && !password.chars().any(|c| c.is_ascii_uppercase()) {
            bail!("Password must contain at least one uppercase letter");
        }
        if self.require_lowercase && !password.chars().any(|c| c.is_ascii_lowercase()) {
            bail!("Password must contain at least one lowercase letter");
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            bail!("Password must contain at least one digit");
        }
        if self.require_special && !password.chars().any(|c| !c.is_alphanumeric()) {
            bail!("Password must contain at least one special character");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_config_defaults() {
        let auth = AuthConfig::default();
        assert!(auth.secret.is_empty());
        assert_eq!(auth.token_expiry, 7200);
        assert_eq!(auth.max_ip_login_attempts, 20);
        assert_eq!(auth.reset_token_expiry, 3600);
    }

    #[test]
    fn auth_reset_token_expiry_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\nreset_token_expiry = 1800\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.reset_token_expiry, 1800);
    }

    #[test]
    fn password_policy_defaults() {
        let policy = PasswordPolicy::default();
        assert_eq!(policy.min_length, 8);
        assert_eq!(policy.max_length, 128);
        assert!(!policy.require_uppercase);
        assert!(!policy.require_lowercase);
        assert!(!policy.require_digit);
        assert!(!policy.require_special);
    }

    #[test]
    fn password_policy_accepts_valid() {
        let policy = PasswordPolicy::default();
        assert!(policy.validate("abcdefgh").is_ok());
        assert!(policy.validate("12345678").is_ok());
    }

    #[test]
    fn password_policy_rejects_too_short() {
        let policy = PasswordPolicy {
            min_length: 8,
            ..Default::default()
        };
        assert!(policy.validate("short").is_err());
        assert!(policy.validate("1234567").is_err());
        assert!(policy.validate("12345678").is_ok());
    }

    #[test]
    fn password_policy_rejects_too_long() {
        let policy = PasswordPolicy {
            max_length: 10,
            ..Default::default()
        };
        assert!(policy.validate("12345678").is_ok());
        assert!(policy.validate("12345678901").is_err());
    }

    #[test]
    fn password_policy_require_uppercase() {
        let policy = PasswordPolicy {
            require_uppercase: true,
            ..Default::default()
        };
        assert!(policy.validate("alllower").is_err());
        assert!(policy.validate("hasUpper1").is_ok());
    }

    #[test]
    fn password_policy_require_lowercase() {
        let policy = PasswordPolicy {
            require_lowercase: true,
            ..Default::default()
        };
        assert!(policy.validate("ALLUPPER").is_err());
        assert!(policy.validate("HASLOWERa").is_ok());
    }

    #[test]
    fn password_policy_require_digit() {
        let policy = PasswordPolicy {
            require_digit: true,
            ..Default::default()
        };
        assert!(policy.validate("nodigits").is_err());
        assert!(policy.validate("hasdigit1").is_ok());
    }

    #[test]
    fn password_policy_require_special() {
        let policy = PasswordPolicy {
            require_special: true,
            ..Default::default()
        };
        assert!(policy.validate("nospecial1").is_err());
        assert!(policy.validate("special!1").is_ok());
    }

    #[test]
    fn password_policy_all_requirements() {
        let policy = PasswordPolicy {
            min_length: 8,
            max_length: 128,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: true,
        };
        assert!(policy.validate("Abc1234!").is_ok());
        assert!(policy.validate("abc1234!").is_err(), "missing uppercase");
        assert!(policy.validate("ABC1234!").is_err(), "missing lowercase");
        assert!(policy.validate("Abcdefg!").is_err(), "missing digit");
        assert!(policy.validate("Abc12345").is_err(), "missing special");
        assert!(policy.validate("Ac1!").is_err(), "too short");
    }

    #[test]
    fn password_policy_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            r#"
[auth.password_policy]
min_length = 12
require_uppercase = true
require_digit = true
"#,
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.password_policy.min_length, 12);
        assert!(config.auth.password_policy.require_uppercase);
        assert!(config.auth.password_policy.require_digit);
        assert!(!config.auth.password_policy.require_lowercase);
        assert!(!config.auth.password_policy.require_special);
    }
}
