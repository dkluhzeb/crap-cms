//! Authentication: JWT secret, lockout/rate-limit policy, session-cookie
//! attributes, and password-strength policy.

mod config;
mod password_policy;
mod secret_file;

pub use config::{AuthConfig, RateLimitBackend, SessionCookieSameSite};
pub use password_policy::PasswordPolicy;
