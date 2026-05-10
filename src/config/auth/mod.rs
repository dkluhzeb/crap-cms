//! Authentication: JWT secret, lockout/rate-limit policy, session-cookie
//! attributes, and password-strength policy.

mod auth_config;
mod password_policy;

pub use auth_config::{AuthConfig, SessionCookieSameSite};
pub use password_policy::PasswordPolicy;
