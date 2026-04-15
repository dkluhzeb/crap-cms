//! Authentication and password policy configuration.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{config::parsing::serde_duration, core::JwtSecret};

/// Controls the `SameSite` attribute of the `crap_session` admin cookie.
///
/// - `Lax` (default) — cookie sent on top-level cross-site navigations (e.g. following a
///   link from an email or external site). Matches browser defaults and is a good balance
///   between usability and CSRF protection.
/// - `Strict` — cookie **never** sent on cross-site requests, including top-level
///   navigations. Hardens the admin against CSRF at the cost of breaking links from
///   external sites / emails: users will appear logged-out after such a navigation and
///   must log in again. Recommended for high-security deployments.
/// - `None` — reserved; not currently supported. `SameSite=None` requires `Secure=true`
///   and cross-site contexts the admin UI doesn't exercise today. Parsing is accepted so
///   that future enablement is a no-migration change; at runtime `None` falls back to
///   `Lax` and emits a warning. Do not rely on this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SessionCookieSameSite {
    /// Cookie sent on same-site requests and top-level cross-site navigations. Default.
    #[default]
    Lax,
    /// Cookie only sent on strictly same-site requests. Breaks cross-site navigation.
    Strict,
    /// Reserved for future use. Currently falls back to `Lax` at runtime.
    None,
}

impl SessionCookieSameSite {
    /// Render the value as the literal used in the `SameSite=` cookie attribute.
    ///
    /// `None` currently falls back to `Lax` — see the enum docs. Callers that need
    /// to detect the configured-but-unsupported case should inspect `self` directly.
    pub fn as_attribute(self) -> &'static str {
        match self {
            SessionCookieSameSite::Strict => "Strict",
            // `None` deliberately falls through to `Lax` for now (see enum docs).
            SessionCookieSameSite::Lax | SessionCookieSameSite::None => "Lax",
        }
    }
}

/// JWT authentication settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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
    /// Rate limit backend: `"memory"` (default), `"redis"`, or `"none"`.
    /// `"redis"` shares rate limit state across servers (requires `--features redis`).
    #[serde(default = "default_rate_limit_backend")]
    pub rate_limit_backend: String,
    /// Redis URL for rate limit backend. Defaults to `cache.redis_url` if empty.
    #[serde(default)]
    pub rate_limit_redis_url: String,
    /// Key prefix for Redis rate limit backend.
    #[serde(default = "default_rate_limit_prefix")]
    pub rate_limit_prefix: String,
    /// Password strength requirements.
    #[serde(default)]
    pub password_policy: PasswordPolicy,
    /// `SameSite` attribute for the `crap_session` admin cookie.
    ///
    /// Default: `"lax"`. Set to `"strict"` to refuse the session cookie on any
    /// cross-site request (including top-level navigations from emails / external
    /// links) for stricter CSRF protection. `"none"` is accepted for forward
    /// compatibility but currently falls back to `lax` at runtime with a warning —
    /// see [`SessionCookieSameSite`].
    #[serde(default)]
    pub session_cookie_samesite: SessionCookieSameSite,
}

fn default_rate_limit_backend() -> String {
    "memory".to_string()
}

fn default_rate_limit_prefix() -> String {
    "crap:rl:".to_string()
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
            rate_limit_backend: default_rate_limit_backend(),
            rate_limit_redis_url: String::new(),
            rate_limit_prefix: default_rate_limit_prefix(),
            password_policy: PasswordPolicy::default(),
            session_cookie_samesite: SessionCookieSameSite::default(),
        }
    }
}

/// Password strength requirements. Applied to all password-setting paths:
/// user creation (admin, gRPC, CLI), password reset, and password update.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PasswordPolicy {
    /// Minimum password length. Default: 8. Recommended: 12+ for modern security.
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
        if password.chars().count() < self.min_length {
            bail!("Password must be at least {} characters", self.min_length);
        }
        // Max length uses byte length intentionally: Argon2 hashes the raw bytes,
        // so limiting bytes prevents DoS via large multi-byte payloads.
        if password.len() > self.max_length {
            bail!("Password must be at most {} bytes", self.max_length);
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

    /// Regression: max_length error message said "characters" but the check uses byte length.
    #[test]
    fn password_policy_max_length_error_says_bytes() {
        let policy = PasswordPolicy {
            max_length: 10,
            ..Default::default()
        };
        let err = policy.validate("12345678901").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bytes"),
            "error message should say 'bytes', got: {}",
            msg
        );
        assert!(
            !msg.contains("characters"),
            "error message should not say 'characters', got: {}",
            msg
        );
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
    fn session_cookie_samesite_default_is_lax() {
        let auth = AuthConfig::default();
        assert_eq!(auth.session_cookie_samesite, SessionCookieSameSite::Lax);
        assert_eq!(auth.session_cookie_samesite.as_attribute(), "Lax");
    }

    #[test]
    fn session_cookie_samesite_parses_from_toml_lowercase() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\nsession_cookie_samesite = \"strict\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(
            config.auth.session_cookie_samesite,
            SessionCookieSameSite::Strict
        );
        assert_eq!(config.auth.session_cookie_samesite.as_attribute(), "Strict");
    }

    #[test]
    fn session_cookie_samesite_rejects_invalid_value() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\nsession_cookie_samesite = \"bogus\"\n",
        )
        .unwrap();
        let err = crate::config::CrapConfig::load(tmp.path())
            .expect_err("bogus samesite value must fail to parse");

        // Walk the full error chain — the top-level anyhow wrapper is a
        // generic "failed to deserialize" string; the specific variant /
        // field name only shows up in the source chain.
        let full = format!("{err:#}").to_lowercase();
        assert!(
            full.contains("samesite")
                || full.contains("bogus")
                || full.contains("variant")
                || full.contains("unknown variant"),
            "expected parse error mentioning the bad variant, got: {full}"
        );
    }

    #[test]
    fn session_cookie_samesite_none_falls_back_to_lax_attribute() {
        // `None` is parseable but currently renders as `Lax` at runtime.
        assert_eq!(SessionCookieSameSite::None.as_attribute(), "Lax");
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
