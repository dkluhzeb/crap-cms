//! Validation of the `[server]` and `[database]` sections.

use std::net::IpAddr;

use ipnet::IpNet;
use tracing::warn;

use crate::config::{CrapConfig, ErrorReport};

impl CrapConfig {
    /// Validate database pool settings.
    pub(in crate::config) fn validate_database(&self, report: &mut ErrorReport) {
        if self.database.pool_max_size == 0 {
            report.push_message("database.pool_max_size must be > 0");
        }

        if self.database.write_pool_max_size == 0 {
            report.push_message("database.write_pool_max_size must be > 0");
        }

        if self.database.connection_timeout == 0 {
            report.push_message("database.connection_timeout must be > 0");
        }
    }

    /// Validate server ports, timeouts, rate limiting, the public URL, the
    /// proxy allowlist and the connection limits.
    pub(in crate::config) fn validate_server(&self, report: &mut ErrorReport) {
        self.validate_ports(report);

        if self.server.grpc_timeout == Some(0) {
            report.push_message("server.grpc_timeout must be > 0 (or omitted to disable)");
        }

        if self.server.grpc_rate_limit_requests > 0 && self.server.grpc_rate_limit_window == 0 {
            report.push_message(
                "server.grpc_rate_limit_window must be > 0 when grpc_rate_limit_requests > 0",
            );
        }

        if self.server.bulk_max_documents < 0 {
            report.push_message("server.bulk_max_documents must be >= 0 (0 = no limit)");
        }

        self.validate_public_url(report);
        self.validate_trusted_proxies(report);
        self.validate_connection_limits(report);
    }

    /// Both ports set, and different — compared only once both are set, so a
    /// zero port is one problem.
    fn validate_ports(&self, report: &mut ErrorReport) {
        if self.server.admin_port == 0 || self.server.grpc_port == 0 {
            report.push_message("Server ports must be > 0");
            return;
        }

        if self.server.admin_port == self.server.grpc_port {
            report.push_message("admin_port and grpc_port must be different");
        }
    }

    /// `server.public_url`, when set, is a non-blank URL with a scheme.
    fn validate_public_url(&self, report: &mut ErrorReport) {
        let Some(url) = &self.server.public_url else {
            return;
        };

        let trimmed = url.trim();
        if trimmed.is_empty() {
            report.push_message(
                "server.public_url must not be blank (omit it to auto-derive from host/port)",
            );
            return;
        }

        if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
            report.push_message(format!(
                "server.public_url must include a scheme (http:// or https://); got {url:?}. \
                 It is used to build absolute links such as password-reset emails, which \
                 break without one."
            ));
        }
    }

    /// Validate the listener and pre-auth limits: each must be positive — a
    /// zero would refuse every connection, stream or login form.
    fn validate_connection_limits(&self, report: &mut ErrorReport) {
        if self.server.auth_body_limit == 0 {
            report.push_message("server.auth_body_limit must be > 0");
        }

        if self.server.max_connections == Some(0) {
            report.push_message("server.max_connections must be > 0 (or omitted to derive it)");
        }

        if self.server.header_read_timeout == 0 {
            report.push_message("server.header_read_timeout must be > 0");
        }

        if self.server.grpc_max_concurrent_streams == 0 {
            report.push_message("server.grpc_max_concurrent_streams must be > 0");
        }

        if self.server.grpc_keepalive_interval == 0 {
            report.push_message("server.grpc_keepalive_interval must be > 0");
        }
    }

    /// Validate `trust_proxy` / `trusted_proxies` pairing.
    ///
    /// Fails startup when `trust_proxy = true` without a `trusted_proxies`
    /// allowlist -- in that state any client can spoof `X-Forwarded-For`
    /// to rotate per-IP rate limits. Operators who genuinely need the
    /// legacy "trust XFF from any peer" behaviour (e.g., local dev
    /// fronted by a test proxy) must opt in explicitly by setting
    /// `trusted_proxies = ["*"]`.
    ///
    /// Also fails on every malformed entry so typos are caught at startup
    /// rather than silently disabling protection.
    fn validate_trusted_proxies(&self, report: &mut ErrorReport) {
        if self.server.trust_proxy && self.server.trusted_proxies.is_empty() {
            report.push_message(
                "server.trust_proxy is enabled without server.trusted_proxies. \
                 Set server.trusted_proxies to the IP(s) or CIDR(s) of your \
                 reverse proxy (e.g. [\"10.0.0.0/8\"]), or set it to [\"*\"] \
                 to explicitly trust any peer (not recommended in production \
                 -- X-Forwarded-For becomes spoofable).",
            );
        }

        for entry in &self.server.trusted_proxies {
            if entry == "*" || entry.parse::<IpNet>().is_ok() || entry.parse::<IpAddr>().is_ok() {
                continue;
            }

            report.push_message(format!(
                "server.trusted_proxies entry {entry:?} is not a valid IP, \
                 CIDR, or the \"*\" wildcard"
            ));
        }

        if self.server.trust_proxy && self.server.trusted_proxies.iter().any(|e| e == "*") {
            warn!(
                "server.trusted_proxies contains \"*\" -- X-Forwarded-For is \
                 honoured from any peer. Use only for development or when \
                 the admin port is not exposed to untrusted networks."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_public_url_without_scheme_errors() {
        let mut config = CrapConfig::default();
        config.server.public_url = Some("example.com".to_string());
        let err = config.validate().unwrap_err().to_string();
        assert!(
            err.contains("public_url") && err.contains("scheme"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn validate_public_url_blank_errors() {
        let mut config = CrapConfig::default();
        config.server.public_url = Some("   ".to_string());
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("public_url"), "unexpected: {err}");
    }

    #[test]
    fn validate_public_url_with_scheme_passes() {
        let mut config = CrapConfig::default();
        config.server.public_url = Some("https://cms.example.com".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_pool_max_size_zero_errors() {
        let mut config = CrapConfig::default();
        config.database.pool_max_size = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("pool_max_size"));
    }

    #[test]
    fn validate_write_pool_max_size_zero_errors() {
        let mut config = CrapConfig::default();
        config.database.write_pool_max_size = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("write_pool_max_size"));
    }

    #[test]
    fn validate_connection_timeout_zero_errors() {
        let mut config = CrapConfig::default();
        config.database.connection_timeout = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("connection_timeout"));
    }

    #[test]
    fn validate_connection_limits_reject_zero() {
        let cases: [fn(&mut CrapConfig); 5] = [
            |c| c.server.auth_body_limit = 0,
            |c| c.server.max_connections = Some(0),
            |c| c.server.header_read_timeout = 0,
            |c| c.server.grpc_max_concurrent_streams = 0,
            |c| c.server.grpc_keepalive_interval = 0,
        ];

        for zero_out in cases {
            let mut config = CrapConfig::default();
            zero_out(&mut config);

            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn validate_admin_port_zero_errors() {
        let mut config = CrapConfig::default();
        config.server.admin_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("ports must be > 0"));
    }

    #[test]
    fn validate_grpc_port_zero_errors() {
        let mut config = CrapConfig::default();
        config.server.grpc_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("ports must be > 0"));
    }

    #[test]
    fn validate_same_ports_errors() {
        let mut config = CrapConfig::default();
        config.server.admin_port = 5000;
        config.server.grpc_port = 5000;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("must be different"));
    }

    #[test]
    fn validate_distinct_nonzero_ports_passes() {
        let mut config = CrapConfig::default();
        config.server.admin_port = 3000;
        config.server.grpc_port = 50051;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_trust_proxy_without_allowlist_errors() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("trusted_proxies"),
            "expected allowlist error, got: {msg}",
        );
        assert!(
            msg.contains("\"*\""),
            "error should mention the explicit-wildcard escape hatch: {msg}",
        );
    }

    #[test]
    fn validate_trust_proxy_with_allowlist_passes() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        config.server.trusted_proxies = vec!["10.0.0.0/8".into(), "127.0.0.1".into()];
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_trust_proxy_with_explicit_wildcard_passes() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        config.server.trusted_proxies = vec!["*".into()];
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_trusted_proxies_rejects_malformed_entry() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        config.server.trusted_proxies = vec!["10.0.0.0/8".into(), "not-an-ip".into()];
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("not-an-ip"));
    }

    #[test]
    fn validate_trust_proxy_disabled_ignores_allowlist_shape() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = false;
        config.server.trusted_proxies = vec!["definitely-not-an-ip".into()];
        assert!(config.validate().is_err());
    }

    /// `0` is the documented "no deadline" setting of both request timeouts.
    #[test]
    fn validate_accepts_zero_request_and_upload_timeouts() {
        let mut config = CrapConfig::default();
        config.server.request_timeout = 0;
        config.server.upload_timeout = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_grpc_timeout_zero() {
        let mut config = CrapConfig::default();
        config.server.grpc_timeout = Some(0);
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("grpc_timeout"));
    }

    #[test]
    fn validate_timeout_none_passes() {
        let mut config = CrapConfig::default();
        config.server.grpc_timeout = None;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_grpc_rate_limit_window_zero() {
        let mut config = CrapConfig::default();
        config.server.grpc_rate_limit_requests = 100;
        config.server.grpc_rate_limit_window = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("grpc_rate_limit_window"));
    }

    #[test]
    fn validate_grpc_rate_limit_window_zero_ok_when_disabled() {
        let mut config = CrapConfig::default();
        config.server.grpc_rate_limit_requests = 0;
        config.server.grpc_rate_limit_window = 0;
        assert!(config.validate().is_ok());
    }

    /// Regression: validation stopped at the first problem of a section, so a
    /// second one in the same section surfaced only after the first was fixed.
    /// Every problem of `[server]` is reported together.
    #[test]
    fn every_server_problem_is_reported() {
        let mut config = CrapConfig::default();
        config.server.grpc_timeout = Some(0);
        config.server.header_read_timeout = 0;
        config.server.trusted_proxies = vec!["nope".into(), "also-nope".into()];

        let err = config.validate().unwrap_err().to_string();

        assert!(err.starts_with("4 problems:"), "{err}");
        assert!(err.contains("grpc_timeout"), "{err}");
        assert!(err.contains("header_read_timeout"), "{err}");
        assert!(
            err.contains("\"nope\"") && err.contains("\"also-nope\""),
            "{err}"
        );
    }
}
