//! Per-section validation methods on [`CrapConfig`]. The public
//! `validate()` orchestrator lives on `types.rs`; each helper here
//! checks one config section in isolation so test cases stay narrow.

use std::{net::IpAddr, str::FromStr};

use anyhow::{Result, bail};
use axum::http::{HeaderName, HeaderValue, Method};
use ipnet::IpNet;
use tracing::warn;

use crate::config::{CacheBackend, CrapConfig};

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

        if self.database.write_pool_max_size == 0 {
            bail!("database.write_pool_max_size must be > 0");
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

        if self.server.bulk_max_documents < 0 {
            bail!("server.bulk_max_documents must be >= 0 (0 = no limit)");
        }

        if let Some(url) = &self.server.public_url {
            let trimmed = url.trim();
            if trimmed.is_empty() {
                bail!(
                    "server.public_url must not be blank (omit it to auto-derive from host/port)"
                );
            }
            if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                bail!(
                    "server.public_url must include a scheme (http:// or https://); got {url:?}. \
                     It is used to build absolute links such as password-reset emails, which \
                     break without one."
                );
            }
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
                    "server.trusted_proxies entry {entry:?} is not a valid IP, \
                     CIDR, or the \"*\" wildcard"
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

    /// Validate the `[cors]` section. Only runs when CORS is enabled
    /// (non-empty `allowed_origins`) — an empty list means the layer is
    /// never built.
    ///
    /// Every entry used to be converted with `filter_map(.parse().ok())`
    /// at layer-build time, silently dropping anything unparseable — and
    /// values that *parse* but can never match a browser `Origin` header
    /// (no scheme, trailing slash/path) weren't caught at all. All of
    /// these are now load-time errors.
    pub(super) fn validate_cors(&self) -> Result<()> {
        let origins = &self.cors.allowed_origins;
        if origins.is_empty() {
            return Ok(());
        }

        let has_wildcard = origins.iter().any(|o| o == "*");
        if has_wildcard && origins.len() > 1 {
            bail!(
                "cors.allowed_origins: \"*\" must be the only entry — mixed with explicit \
                 origins it is matched literally and never allows anything"
            );
        }

        if has_wildcard && self.cors.allow_credentials {
            bail!(
                "cors.allow_credentials = true is incompatible with the wildcard origin \
                 \"*\" (forbidden by the CORS spec). List explicit origins instead."
            );
        }

        for origin in origins.iter().filter(|o| *o != "*") {
            Self::validate_cors_origin(origin)?;
        }

        for method in &self.cors.allowed_methods {
            if Method::from_str(method).is_err() {
                bail!("cors.allowed_methods entry {method:?} is not a valid HTTP method token");
            }
        }

        for (list, header) in std::iter::empty()
            .chain(self.cors.allowed_headers.iter().map(|h| ("allowed", h)))
            .chain(self.cors.exposed_headers.iter().map(|h| ("exposed", h)))
        {
            if HeaderName::from_str(header).is_err() {
                bail!("cors.{list}_headers entry {header:?} is not a valid header name");
            }
        }

        Ok(())
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
                anyhow::anyhow!(
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
        // `serde_json`'s default deserialization recursion limit — the round-trip
        // ceiling for stored JSON (see the `max_nesting_depth` check below).
        const SERDE_RECURSION_LIMIT: usize = 128;

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

        if self.depth.max_nesting_depth == 0 {
            bail!("depth.max_nesting_depth must be >= 1 (0 rejects all nested data)");
        }

        // The data-nesting ceiling must accommodate the data that population
        // produces: a document populated to `max_depth` nests at least that
        // deep, so a smaller ceiling would reject your own legitimately-deep
        // data at Lua↔JSON conversion time.
        if let Ok(max_depth) = usize::try_from(self.depth.max_depth)
            && self.depth.max_nesting_depth < max_depth
        {
            warn!(
                "depth.max_nesting_depth ({}) is below depth.max_depth ({}) -- data populated to max_depth may exceed the nesting limit and fail to convert",
                self.depth.max_nesting_depth, self.depth.max_depth
            );
        }

        // Data nested deeper than serde's parse limit can be built in memory and
        // serialized, but cannot be parsed back from stored JSON — effectively
        // write-only. Going beyond it would require a custom `Deserializer`
        // recursion limit at every user-JSON parse site.
        if self.depth.max_nesting_depth > SERDE_RECURSION_LIMIT {
            warn!(
                "depth.max_nesting_depth ({}) exceeds the JSON parser recursion limit ({SERDE_RECURSION_LIMIT}) -- data nested deeper than {SERDE_RECURSION_LIMIT} can be built but will not parse back from stored JSON",
                self.depth.max_nesting_depth
            );
        }

        Ok(())
    }

    /// Validate job scheduler settings.
    pub(super) fn validate_jobs(&self) -> Result<()> {
        if self.hooks.vm_pool_size == 0 {
            bail!("hooks.vm_pool_size must be > 0");
        }

        if self.hooks.max_vm_pool_size == 0 {
            bail!("hooks.max_vm_pool_size must be > 0");
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
        // `0` means "no cap" -- the default, silent. Finite values longer
        // than 30 days deserve a nudge, since they materially widen the
        // window in which a stolen session token is usable.
        const SESSION_MAX_AGE_WARN_THRESHOLD: u64 = 30 * 86400;

        if !self.auth.secret.is_empty() && self.auth.secret.len() < 32 {
            warn!("auth.secret is shorter than 32 characters -- consider using a stronger key");
        }

        if self.auth.password_policy.min_length > self.auth.password_policy.max_length {
            bail!(
                "auth.password_policy.min_length ({}) must be <= auth.password_policy.max_length ({})",
                self.auth.password_policy.min_length,
                self.auth.password_policy.max_length
            );
        }

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
    pub(super) fn validate_cache(&self) {
        if self.cache.backend == CacheBackend::Memory && self.cache.max_entries == 0 {
            warn!(
                "cache.max_entries = 0 with memory backend -- cache will never store entries (equivalent to backend = \"none\")"
            );
        }
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
    fn validate_max_nesting_depth_zero_errors() {
        let mut config = CrapConfig::default();
        config.depth.max_nesting_depth = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_nesting_depth"));
    }

    #[test]
    fn validate_max_nesting_depth_below_max_depth_warns_but_passes() {
        // A nesting ceiling under the population depth is a misconfiguration
        // (populated data could exceed it) but is surfaced as a warning, not a
        // hard failure.
        let mut config = CrapConfig::default();
        config.depth.max_depth = 20;
        config.depth.max_nesting_depth = 5;
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
            "Expected mcp.api_key error, got: {err}"
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
            "Expected short-key error, got: {msg}",
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
