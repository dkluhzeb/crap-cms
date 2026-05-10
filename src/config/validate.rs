//! Per-section validation methods on [`CrapConfig`]. The public
//! `validate()` orchestrator lives on `types.rs`; each helper here
//! checks one config section in isolation so test cases stay narrow.

use std::net::IpAddr;

use anyhow::{Result, bail};
use ipnet::IpNet;
use tracing::warn;

use crate::config::CrapConfig;

/// Minimum character length for `mcp.api_key` when `mcp.http` is enabled.
/// 32 characters of the typical `base64`/`hex` alphabets give >= 128 bits of
/// entropy even with low per-char entropy -- well past what brute-force can
/// reach against a key that an attacker cannot guess from context.
const MIN_MCP_API_KEY_LEN: usize = 32;

impl CrapConfig {
    /// Validate database pool settings.
    pub(super) fn validate_database(&self) -> Result<()> {
        if self.database.pool_max_size == 0 {
            bail!("database.pool_max_size must be > 0");
        }

        if self.database.connection_timeout == 0 {
            bail!("database.connection_timeout must be > 0");
        }

        Ok(())
    }

    /// Validate server ports, timeouts, and rate limiting.
    pub(super) fn validate_server(&self) -> Result<()> {
        if self.server.admin_port == 0 || self.server.grpc_port == 0 {
            bail!("Server ports must be > 0");
        }

        if self.server.admin_port == self.server.grpc_port {
            bail!("admin_port and grpc_port must be different");
        }

        if self.server.request_timeout == Some(0) {
            bail!("server.request_timeout must be > 0 (or omitted to disable)");
        }

        if self.server.grpc_timeout == Some(0) {
            bail!("server.grpc_timeout must be > 0 (or omitted to disable)");
        }

        if self.server.grpc_rate_limit_requests > 0 && self.server.grpc_rate_limit_window == 0 {
            bail!("server.grpc_rate_limit_window must be > 0 when grpc_rate_limit_requests > 0");
        }

        self.validate_trusted_proxies()?;

        Ok(())
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
    /// Also fails on malformed entries so typos are caught at startup
    /// rather than silently disabling protection.
    fn validate_trusted_proxies(&self) -> Result<()> {
        if self.server.trust_proxy && self.server.trusted_proxies.is_empty() {
            bail!(
                "server.trust_proxy is enabled without server.trusted_proxies. \
                 Set server.trusted_proxies to the IP(s) or CIDR(s) of your \
                 reverse proxy (e.g. [\"10.0.0.0/8\"]), or set it to [\"*\"] \
                 to explicitly trust any peer (not recommended in production \
                 -- X-Forwarded-For becomes spoofable)."
            );
        }

        for entry in &self.server.trusted_proxies {
            if entry == "*" {
                continue;
            }

            if entry.parse::<IpNet>().is_err() && entry.parse::<IpAddr>().is_err() {
                bail!(
                    "server.trusted_proxies entry {:?} is not a valid IP, \
                     CIDR, or the \"*\" wildcard",
                    entry
                );
            }
        }

        if self.server.trust_proxy && self.server.trusted_proxies.iter().any(|e| e == "*") {
            warn!(
                "server.trusted_proxies contains \"*\" -- X-Forwarded-For is \
                 honoured from any peer. Use only for development or when \
                 the admin port is not exposed to untrusted networks."
            );
        }

        Ok(())
    }

    /// Validate pagination limits.
    pub(super) fn validate_pagination(&self) -> Result<()> {
        if self.pagination.default_limit <= 0 {
            bail!("pagination.default_limit must be > 0");
        }

        if self.pagination.max_limit <= 0 {
            bail!("pagination.max_limit must be > 0");
        }

        if self.pagination.default_limit > self.pagination.max_limit {
            bail!(
                "pagination.default_limit ({}) must be <= pagination.max_limit ({})",
                self.pagination.default_limit,
                self.pagination.max_limit
            );
        }

        Ok(())
    }

    /// Validate depth/population limits.
    pub(super) fn validate_depth(&self) -> Result<()> {
        if self.depth.default_depth < 0 {
            bail!("depth.default_depth must be >= 0");
        }

        if self.depth.max_depth < 0 {
            bail!("depth.max_depth must be >= 0");
        }

        if self.depth.max_depth == 0 {
            warn!("depth.max_depth = 0 -- all depth/populate requests will be capped to 0");
        }

        if self.depth.default_depth > self.depth.max_depth {
            warn!(
                "depth.default_depth ({}) exceeds depth.max_depth ({}) -- requests will be capped",
                self.depth.default_depth, self.depth.max_depth
            );
        }

        Ok(())
    }

    /// Validate job scheduler settings.
    pub(super) fn validate_jobs(&self) -> Result<()> {
        if self.hooks.vm_pool_size == 0 {
            bail!("hooks.vm_pool_size must be > 0");
        }

        if self.jobs.max_concurrent == 0 {
            warn!("jobs.max_concurrent = 0 -- no jobs will be executed");
        }

        if self.jobs.poll_interval == 0 {
            bail!("jobs.poll_interval must be > 0");
        }

        if self.jobs.cron_interval == 0 {
            bail!("jobs.cron_interval must be > 0");
        }

        if self.jobs.heartbeat_interval == 0 {
            bail!("jobs.heartbeat_interval must be > 0");
        }

        Ok(())
    }

    /// Validate auth and password policy settings.
    pub(super) fn validate_auth(&self) -> Result<()> {
        if !self.auth.secret.is_empty() && self.auth.secret.len() < 32 {
            warn!("auth.secret is shorter than 32 characters -- consider using a stronger key");
        }

        if self.auth.password_policy.min_length > self.auth.password_policy.max_length {
            bail!(
                "auth.password.min_length ({}) must be <= auth.password.max_length ({})",
                self.auth.password_policy.min_length,
                self.auth.password_policy.max_length
            );
        }

        // `0` means "no cap" -- the default, silent. Finite values longer
        // than 30 days deserve a nudge, since they materially widen the
        // window in which a stolen session token is usable.
        const SESSION_MAX_AGE_WARN_THRESHOLD: u64 = 30 * 86400;

        if self.auth.session_absolute_max_age > SESSION_MAX_AGE_WARN_THRESHOLD {
            warn!(
                "auth.session_absolute_max_age is {} seconds (> 30 days) -- \
                 long caps enlarge the window in which a stolen session token \
                 remains valid. Consider shortening, or pair with step-up \
                 authentication for sensitive operations.",
                self.auth.session_absolute_max_age,
            );
        }

        Ok(())
    }

    /// Validate email/SMTP settings.
    pub(super) fn validate_email(&self) -> Result<()> {
        if !self.email.smtp_host.is_empty() && self.email.smtp_port == 0 {
            bail!("email.smtp_port must be > 0 when smtp_host is configured");
        }

        Ok(())
    }

    /// Validate logging settings.
    pub(super) fn validate_logging(&self) -> Result<()> {
        if self.logging.file && self.logging.path.is_empty() {
            bail!("logging.path must not be empty when file logging is enabled");
        }

        if self.logging.file && self.logging.max_files == 0 {
            warn!("logging.max_files = 0 -- all rotated log files will be deleted on startup");
        }

        Ok(())
    }

    /// Validate MCP settings.
    ///
    /// When `mcp.http = true`, enforces both presence and a minimum length
    /// on `mcp.api_key`. MCP operates with `overrideAccess = true` semantics
    /// (collection- and field-level ACLs are bypassed), so a weak transport
    /// key exposes the entire dataset -- a 32-byte floor keeps brute-force
    /// infeasible for realistic attacker budgets.
    pub(super) fn validate_mcp(&self) -> Result<()> {
        if !(self.mcp.enabled && self.mcp.http) {
            return Ok(());
        }

        if self.mcp.api_key.is_empty() {
            bail!(
                "mcp.http is enabled without an API key -- \
                 set mcp.api_key in crap.toml to secure the MCP HTTP endpoint"
            );
        }

        if self.mcp.api_key.as_ref().len() < MIN_MCP_API_KEY_LEN {
            bail!(
                "mcp.api_key is too short ({} chars) -- require at least {} \
                 characters. MCP bypasses collection and field ACLs, so a \
                 short key risks exposing the entire dataset. Generate one \
                 with `openssl rand -hex 32` or `head -c 32 /dev/urandom | base64`.",
                self.mcp.api_key.as_ref().len(),
                MIN_MCP_API_KEY_LEN,
            );
        }

        Ok(())
    }

    /// Validate live event streaming settings.
    pub(super) fn validate_live(&self) -> Result<()> {
        if self.live.enabled && self.live.channel_capacity == 0 {
            bail!("live.channel_capacity must be > 0 when live events are enabled");
        }

        Ok(())
    }

    /// Validate cache settings.
    pub(super) fn validate_cache(&self) -> Result<()> {
        if self.cache.backend == "memory" && self.cache.max_entries == 0 {
            warn!(
                "cache.max_entries = 0 with memory backend -- cache will never store entries (equivalent to backend = \"none\")"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_default_config_passes() {
        let config = CrapConfig::default();
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
    fn validate_connection_timeout_zero_errors() {
        let mut config = CrapConfig::default();
        config.database.connection_timeout = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("connection_timeout"));
    }

    #[test]
    fn validate_vm_pool_size_zero_errors() {
        let mut config = CrapConfig::default();
        config.hooks.vm_pool_size = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("vm_pool_size"));
    }

    #[test]
    fn validate_max_concurrent_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.jobs.max_concurrent = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_poll_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.poll_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("poll_interval"));
    }

    #[test]
    fn validate_cron_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.cron_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("cron_interval"));
    }

    #[test]
    fn validate_heartbeat_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.heartbeat_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("heartbeat_interval"));
    }

    #[test]
    fn validate_short_auth_secret_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.auth.secret = crate::core::JwtSecret::new("short");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_max_depth_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.depth.max_depth = 0;
        assert!(config.validate().is_ok());
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
    fn validate_logging_empty_path_errors() {
        let mut config = CrapConfig::default();
        config.logging.file = true;
        config.logging.path = String::new();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("logging.path"));
    }

    #[test]
    fn validate_logging_max_files_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.logging.file = true;
        config.logging.max_files = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_logging_disabled_empty_path_passes() {
        let mut config = CrapConfig::default();
        config.logging.file = false;
        config.logging.path = String::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_mcp_http_without_api_key_errors() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("mcp.api_key"),
            "Expected mcp.api_key error, got: {}",
            err
        );
    }

    #[test]
    fn validate_mcp_http_with_strong_api_key_passes() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        config.mcp.api_key = crate::config::McpApiKey::from("0123456789abcdef0123456789abcdef");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_mcp_http_with_short_api_key_errors() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        config.mcp.api_key = crate::config::McpApiKey::from("secret-key-1234");
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("too short"),
            "Expected short-key error, got: {}",
            msg,
        );
        assert!(msg.contains("openssl rand") || msg.contains("/dev/urandom"));
    }

    #[test]
    fn validate_mcp_disabled_no_api_key_passes() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = false;
        config.mcp.http = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_mcp_stdio_no_api_key_passes() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = false;
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

    #[test]
    fn validate_rejects_smtp_port_zero() {
        let mut config = CrapConfig::default();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.smtp_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("smtp_port"));
    }

    #[test]
    fn validate_smtp_port_zero_ok_when_host_empty() {
        let mut config = CrapConfig::default();
        config.email.smtp_host = String::new();
        config.email.smtp_port = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_request_timeout_zero() {
        let mut config = CrapConfig::default();
        config.server.request_timeout = Some(0);
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("request_timeout"));
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
        config.server.request_timeout = None;
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

    #[test]
    fn validate_channel_capacity_zero_errors() {
        let mut config = CrapConfig::default();
        config.live.channel_capacity = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("channel_capacity"));
    }

    #[test]
    fn validate_channel_capacity_zero_ok_when_live_disabled() {
        let mut config = CrapConfig::default();
        config.live.enabled = false;
        config.live.channel_capacity = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_pagination_default_limit_zero_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
    }

    #[test]
    fn validate_pagination_default_limit_negative_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = -5;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
    }

    #[test]
    fn validate_pagination_max_limit_zero_errors() {
        let mut config = CrapConfig::default();
        config.pagination.max_limit = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_limit"));
    }

    #[test]
    fn validate_pagination_default_exceeds_max_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = 100;
        config.pagination.max_limit = 50;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
        assert!(err.to_string().contains("max_limit"));
    }

    #[test]
    fn validate_depth_negative_default_errors() {
        let mut config = CrapConfig::default();
        config.depth.default_depth = -1;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_depth"));
    }

    #[test]
    fn validate_depth_negative_max_errors() {
        let mut config = CrapConfig::default();
        config.depth.max_depth = -1;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_depth"));
    }
}
