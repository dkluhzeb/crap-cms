//! Authentication: JWT secret, lockout/rate-limit policy, session-cookie
//! attributes, and password-strength policy.

mod config;
mod password_policy;
mod secret_file;

pub use config::{AuthConfig, DEFAULT_TOKEN_EXPIRY, RateLimitBackend, SessionCookieSameSite};
pub use password_policy::{PasswordPolicy, PasswordViolation};
pub(crate) use secret_file::write_new_owner_only;
