//! CORS (Cross-Origin Resource Sharing) configuration.

use std::{str::FromStr, time::Duration};

use axum::http::{HeaderName, Method};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tracing::warn;

use crate::config::parsing::serde_duration;

/// CORS configuration.
/// Empty `allowed_origins` = CORS layer not added (default, backward compatible).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorsConfig {
    /// Origins allowed to make cross-origin requests. Empty = CORS disabled.
    /// Use `["*"]` to allow any origin (not compatible with `allow_credentials`).
    pub allowed_origins: Vec<String>,
    /// HTTP methods allowed in CORS requests.
    pub allowed_methods: Vec<String>,
    /// Request headers allowed in CORS requests.
    pub allowed_headers: Vec<String>,
    /// Response headers exposed to the browser.
    pub exposed_headers: Vec<String>,
    /// How long browsers can cache preflight results.
    /// Accepts integer seconds or human-readable string ("1h", "3600s").
    #[serde(with = "serde_duration")]
    pub max_age: u64,
    /// Whether to allow credentials (cookies, Authorization header).
    /// Cannot be used with `allowed_origins = ["*"]`.
    pub allow_credentials: bool,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allowed_methods: vec![
                "GET".into(),
                "POST".into(),
                "PUT".into(),
                "DELETE".into(),
                "PATCH".into(),
                "OPTIONS".into(),
            ],
            allowed_headers: vec!["Content-Type".into(), "Authorization".into()],
            exposed_headers: Vec::new(),
            max_age: 3600,
            allow_credentials: false,
        }
    }
}

impl CorsConfig {
    /// Build a tower-http CorsLayer from this config. Returns None if no origins configured.
    pub fn build_layer(&self) -> Option<CorsLayer> {
        if self.allowed_origins.is_empty() {
            return None;
        }

        let is_wildcard = self.allowed_origins.len() == 1 && self.allowed_origins[0] == "*";

        // Validate: wildcard + credentials is invalid per CORS spec
        if is_wildcard && self.allow_credentials {
            warn!(
                "CORS: allow_credentials is incompatible with wildcard origin '*'. \
                 Ignoring allow_credentials."
            );
        }

        let origin = if is_wildcard {
            AllowOrigin::any()
        } else {
            AllowOrigin::list(self.allowed_origins.iter().filter_map(|o| o.parse().ok()))
        };

        let methods = AllowMethods::list(
            self.allowed_methods
                .iter()
                .filter_map(|m| Method::from_str(m).ok()),
        );

        let headers = AllowHeaders::list(
            self.allowed_headers
                .iter()
                .filter_map(|h| HeaderName::from_str(h).ok()),
        );

        let mut layer = CorsLayer::new()
            .allow_origin(origin)
            .allow_methods(methods)
            .allow_headers(headers)
            .max_age(Duration::from_secs(self.max_age));

        if !self.exposed_headers.is_empty() {
            layer = layer.expose_headers(
                self.exposed_headers
                    .iter()
                    .filter_map(|h| HeaderName::from_str(h).ok())
                    .collect::<Vec<_>>(),
            );
        }

        // Only set credentials when not using wildcard origin
        if self.allow_credentials && !is_wildcard {
            layer = layer.allow_credentials(true);
        }

        Some(layer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_config_defaults() {
        let cors = CorsConfig::default();
        assert!(cors.allowed_origins.is_empty());
        assert_eq!(
            cors.allowed_methods,
            vec!["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"]
        );
        assert_eq!(cors.allowed_headers, vec!["Content-Type", "Authorization"]);
        assert!(cors.exposed_headers.is_empty());
        assert_eq!(cors.max_age, 3600);
        assert!(!cors.allow_credentials);
    }

    #[test]
    fn cors_build_layer_disabled_when_no_origins() {
        let cors = CorsConfig::default();
        assert!(cors.build_layer().is_none());
    }

    #[test]
    fn cors_build_layer_wildcard() {
        let cors = CorsConfig {
            allowed_origins: vec!["*".to_string()],
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_specific_origins() {
        let cors = CorsConfig {
            allowed_origins: vec![
                "http://localhost:3000".to_string(),
                "https://example.com".to_string(),
            ],
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_with_credentials() {
        let cors = CorsConfig {
            allowed_origins: vec!["https://example.com".to_string()],
            allow_credentials: true,
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_wildcard_with_credentials_ignores_credentials() {
        let cors = CorsConfig {
            allowed_origins: vec!["*".to_string()],
            allow_credentials: true,
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_with_exposed_headers() {
        let cors = CorsConfig {
            allowed_origins: vec!["https://example.com".to_string()],
            exposed_headers: vec!["X-Custom-Header".to_string()],
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            r#"
[cors]
allowed_origins = ["https://example.com", "https://app.example.com"]
allowed_methods = ["GET", "POST"]
allowed_headers = ["Content-Type", "Authorization", "X-Custom"]
exposed_headers = ["X-Request-Id"]
max_age = 7200
allow_credentials = true
"#,
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(
            config.cors.allowed_origins,
            vec!["https://example.com", "https://app.example.com"]
        );
        assert_eq!(config.cors.allowed_methods, vec!["GET", "POST"]);
        assert_eq!(
            config.cors.allowed_headers,
            vec!["Content-Type", "Authorization", "X-Custom"]
        );
        assert_eq!(config.cors.exposed_headers, vec!["X-Request-Id"]);
        assert_eq!(config.cors.max_age, 7200);
        assert!(config.cors.allow_credentials);
    }
}
