//! Authentication configuration for collections.

use serde::{Deserialize, Serialize};

/// A custom authentication strategy (name + Lua function reference).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStrategy {
    /// Name of the authentication strategy.
    pub name: String,
    /// Lua function ref (module.function format)
    pub authenticate: String,
}

impl AuthStrategy {
    /// Create a new authentication strategy.
    pub fn new(name: impl Into<String>, authenticate: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            authenticate: authenticate.into(),
        }
    }
}

/// Authentication configuration for a collection (JWT, strategies, local login).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Auth {
    /// Whether authentication is enabled for this collection.
    pub enabled: bool,
    /// JWT token expiry in seconds. Default: 7200 (2 hours).
    #[serde(default = "default_token_expiry")]
    pub token_expiry: u64,
    /// List of custom authentication strategies.
    #[serde(default)]
    pub strategies: Vec<AuthStrategy>,
    /// Whether to disable local (email/password) login.
    #[serde(default)]
    pub disable_local: bool,
    /// Enable email verification requirement for new users. Default: false.
    #[serde(default)]
    pub verify_email: bool,
    /// Enable forgot password flow. Default: true (when auth enabled).
    #[serde(default = "default_true_auth")]
    pub forgot_password: bool,
}

fn default_true_auth() -> bool {
    true
}

fn default_token_expiry() -> u64 {
    7200
}

impl Auth {
    /// Create a new authentication configuration with the given enabled status.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Default::default()
        }
    }
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            enabled: false,
            token_expiry: default_token_expiry(),
            strategies: Vec::new(),
            disable_local: false,
            verify_email: false,
            forgot_password: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_auth_defaults() {
        let auth = Auth::default();
        assert!(!auth.enabled);
        assert_eq!(auth.token_expiry, 7200);
        assert!(auth.strategies.is_empty());
        assert!(!auth.disable_local);
        assert!(!auth.verify_email);
        assert!(auth.forgot_password);
    }
}
