//! Validation of the `[cors]` section.

use std::{iter, str::FromStr};

use anyhow::{Result, anyhow, bail};
use axum::http::{HeaderName, HeaderValue, Method};

use crate::config::{CrapConfig, ErrorReport};

impl CrapConfig {
    /// Validate the `[cors]` section. Only runs when CORS is enabled
    /// (non-empty `allowed_origins`) — an empty list means the layer is
    /// never built.
    ///
    /// Every entry used to be converted with `filter_map(.parse().ok())`
    /// at layer-build time, silently dropping anything unparseable — and
    /// values that *parse* but can never match a browser `Origin` header
    /// (no scheme, trailing slash/path) weren't caught at all. All of
    /// these are load-time errors, each entry reported on its own.
    pub(in crate::config) fn validate_cors(&self, report: &mut ErrorReport) {
        let origins = &self.cors.allowed_origins;
        if origins.is_empty() {
            return;
        }

        let has_wildcard = origins.iter().any(|o| o == "*");
        if has_wildcard && origins.len() > 1 {
            report.push_message(
                "cors.allowed_origins: \"*\" must be the only entry — mixed with explicit \
                 origins it is matched literally and never allows anything",
            );
        }

        if has_wildcard && self.cors.allow_credentials {
            report.push_message(
                "cors.allow_credentials = true is incompatible with the wildcard origin \
                 \"*\" (forbidden by the CORS spec). List explicit origins instead.",
            );
        }

        for origin in origins.iter().filter(|o| *o != "*") {
            report.check(Self::validate_cors_origin(origin));
        }

        self.validate_cors_tokens(report);
    }

    /// Every listed method is an HTTP method token, every listed header a
    /// header name.
    fn validate_cors_tokens(&self, report: &mut ErrorReport) {
        for method in &self.cors.allowed_methods {
            if Method::from_str(method).is_err() {
                report.push_message(format!(
                    "cors.allowed_methods entry {method:?} is not a valid HTTP method token"
                ));
            }
        }

        let headers = iter::empty()
            .chain(self.cors.allowed_headers.iter().map(|h| ("allowed", h)))
            .chain(self.cors.exposed_headers.iter().map(|h| ("exposed", h)));

        for (list, header) in headers {
            if HeaderName::from_str(header).is_err() {
                report.push_message(format!(
                    "cors.{list}_headers entry {header:?} is not a valid header name"
                ));
            }
        }
    }

    /// Validate a single explicit CORS origin: it must be exactly what a
    /// browser sends in the `Origin` header (`scheme://host[:port]`, no
    /// path, no trailing slash) or it will never match.
    fn validate_cors_origin(origin: &str) -> Result<()> {
        if HeaderValue::from_str(origin).is_err() || origin.chars().any(char::is_whitespace) {
            bail!("cors.allowed_origins entry {origin:?} is not a valid header value");
        }

        let rest = origin
            .strip_prefix("https://")
            .or_else(|| origin.strip_prefix("http://"))
            .ok_or_else(|| {
                anyhow!(
                    "cors.allowed_origins entry {origin:?} must include a scheme \
                     (http:// or https://) — browsers send the full origin, so a \
                     schemeless entry never matches"
                )
            })?;

        if rest.is_empty() {
            bail!("cors.allowed_origins entry {origin:?} has no host");
        }

        if rest.contains('/') {
            bail!(
                "cors.allowed_origins entry {origin:?} must not contain a path or \
                 trailing slash — the browser `Origin` header is scheme://host[:port] \
                 only, so this entry would never match"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_cors_schemeless_origin_errors() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec!["example.com".to_string()];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("scheme"), "unexpected: {err}");
    }

    #[test]
    fn validate_cors_origin_with_path_errors() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec!["https://example.com/".to_string()];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("path or"), "unexpected: {err}");
    }

    #[test]
    fn validate_cors_wildcard_mixed_with_origins_errors() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec!["*".to_string(), "https://x.com".to_string()];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("only entry"), "unexpected: {err}");
    }

    #[test]
    fn validate_cors_wildcard_with_credentials_errors() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec!["*".to_string()];
        config.cors.allow_credentials = true;
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("allow_credentials"), "unexpected: {err}");
    }

    #[test]
    fn validate_cors_invalid_header_name_errors() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec!["https://example.com".to_string()];
        config.cors.allowed_headers = vec!["X Custom".to_string()];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("header name"), "unexpected: {err}");
    }

    #[test]
    fn validate_cors_invalid_method_errors() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec!["https://example.com".to_string()];
        config.cors.allowed_methods = vec!["GE T".to_string()];
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("HTTP method"), "unexpected: {err}");
    }

    #[test]
    fn validate_cors_valid_config_passes() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec![
            "https://example.com".to_string(),
            "http://localhost:5173".to_string(),
        ];
        config.cors.allow_credentials = true;
        assert!(config.validate().is_ok());

        config.cors.allowed_origins = vec!["*".to_string()];
        config.cors.allow_credentials = false;
        assert!(config.validate().is_ok());
    }

    /// Every bad entry of `[cors]` is reported, not only the first.
    #[test]
    fn every_cors_problem_is_reported() {
        let mut config = CrapConfig::default();
        config.cors.allowed_origins = vec!["example.com".into(), "https://x.com/".into()];
        config.cors.allowed_methods = vec!["GE T".into()];

        let err = config.validate().unwrap_err().to_string();

        assert!(err.starts_with("3 problems:"), "{err}");
        assert!(err.contains("scheme") && err.contains("path or"), "{err}");
        assert!(err.contains("HTTP method"), "{err}");
    }
}
