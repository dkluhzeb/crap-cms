//! JWT secret, lockout policy, password-reset rate limit, and session-cookie shape.

use serde::{Deserialize, Serialize};

use crate::{config::parsing::serde_duration, core::JwtSecret};

use super::password_policy::PasswordPolicy;

/// Controls the `SameSite` attribute of the `crap_session` admin cookie.
///
/// - `Lax` (default) -- cookie sent on top-level cross-site navigations (e.g. following a
///   link from an email or external site). Matches browser defaults and is a good balance
///   between usability and CSRF protection.
/// - `Strict` -- cookie **never** sent on cross-site requests, including top-level
///   navigations. Hardens the admin against CSRF at the cost of breaking links from
///   external sites / emails: users will appear logged-out after such a navigation and
///   must log in again. Recommended for high-security deployments.
/// - `None` -- reserved; not currently supported. `SameSite=None` requires `Secure=true`
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
    /// `None` currently falls back to `Lax` -- see the enum docs. Callers that need
    /// to detect the configured-but-unsupported case should inspect `self` directly.
    #[must_use]
    pub fn as_attribute(self) -> &'static str {
        match self {
            SessionCookieSameSite::Strict => "Strict",
            // `None` deliberately falls through to `Lax` for now (see enum docs).
            SessionCookieSameSite::Lax | SessionCookieSameSite::None => "Lax",
        }
    }
}

/// Rate-limit state backend for auth (login/forgot-password) throttling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitBackend {
    /// In-process counters (the default). Per-server, not shared.
    #[default]
    Memory,
    /// Redis-backed counters shared across servers (requires the `redis` feature).
    Redis,
    /// Rate limiting disabled.
    None,
}

/// JWT authentication settings.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
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
    /// Rate limit backend: `memory` (default), `redis`, or `none`.
    /// `redis` shares rate limit state across servers (requires `--features redis`).
    pub rate_limit_backend: RateLimitBackend,
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
    /// compatibility but currently falls back to `lax` at runtime with a warning --
    /// see [`SessionCookieSameSite`].
    #[serde(default)]
    pub session_cookie_samesite: SessionCookieSameSite,
    /// Hard ceiling on the wall-clock lifetime of a session measured from
    /// the original login, regardless of how many times the token has been
    /// refreshed. Default: `2592000` (30 days). Set to `0` to disable the
    /// cap -- a session then remains valid until `token_expiry` elapses
    /// without a refresh, or the user changes password. Accepts integer
    /// seconds or human strings (`"30d"`, `"12h"`).
    ///
    /// Values greater than 30 days emit a startup warning, since long caps
    /// materially enlarge the window a stolen token stays usable. Long
    /// internal-tool sessions are a legitimate use-case -- the warning is
    /// a reminder, not a block.
    #[serde(default = "default_session_absolute_max_age", with = "serde_duration")]
    pub session_absolute_max_age: u64,
}

fn default_session_absolute_max_age() -> u64 {
    30 * 86400
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
            rate_limit_backend: RateLimitBackend::default(),
            rate_limit_redis_url: String::new(),
            rate_limit_prefix: default_rate_limit_prefix(),
            password_policy: PasswordPolicy::default(),
            session_cookie_samesite: SessionCookieSameSite::default(),
            session_absolute_max_age: default_session_absolute_max_age(),
        }
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

        // Walk the full error chain -- the top-level anyhow wrapper is a
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
}
