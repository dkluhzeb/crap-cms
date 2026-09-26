//! Validation of the `[auth]`, `[email]`, `[logging]`, `[mcp]`, `[live]` and
//! `[cache]` sections.

use tracing::warn;

use crate::config::{CacheBackend, CrapConfig, DatabaseBackend, ErrorReport};

/// Minimum character length for `mcp.api_key` when `mcp.http` is enabled.
/// 32 characters of the typical `base64`/`hex` alphabets give >= 128 bits of
/// entropy even with low per-char entropy -- well past what brute-force can
/// reach against a key that an attacker cannot guess from context.
const MIN_MCP_API_KEY_LEN: usize = 32;

/// Ceiling for `[mcp] max_batch_members`. Generous for any real client —
/// batching exists to save round trips, not to move bulk work — while still
/// bounding how far one request can be multiplied.
const MAX_MCP_BATCH_MEMBERS: usize = 500;

/// `0` means "no cap" -- the default, silent. Finite values longer than 30
/// days deserve a nudge, since they materially widen the window in which a
/// stolen session token is usable.
const SESSION_MAX_AGE_WARN_THRESHOLD: u64 = 30 * 86400;

impl CrapConfig {
    /// Validate auth and password policy settings.
    pub(in crate::config) fn validate_auth(&self, report: &mut ErrorReport) {
        if !self.auth.secret.is_empty() && self.auth.secret.len() < 32 {
            warn!("auth.secret is shorter than 32 characters -- consider using a stronger key");
        }

        self.validate_auth_secret_scope(report);

        // Every auth collection without its own `token_expiry` inherits this
        // lifetime; `0` would mint sessions that are dead on arrival.
        if self.auth.token_expiry == 0 {
            report.push_message("auth.token_expiry must be > 0");
        }

        let policy = &self.auth.password_policy;
        if policy.min_length > policy.max_length {
            report.push_message(format!(
                "auth.password_policy.min_length ({}) must be <= auth.password_policy.max_length ({})",
                policy.min_length, policy.max_length
            ));
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
    }

    /// An unset secret is generated per node and written to that node's
    /// local `data/.jwt_secret`. Across several nodes each would mint its
    /// own: a session issued by one is rejected by the next, and whatever
    /// one node wrote to the shared database under its key — MFA digests,
    /// sealed TOTP secrets, `crap.crypto` ciphertext — is unreadable on the
    /// rest. This runs before `resolve_secret`, so empty means unconfigured.
    fn validate_auth_secret_scope(&self, report: &mut ErrorReport) {
        if !self.auth.secret.is_empty() {
            return;
        }

        if self.has_multi_node_signal() {
            report.push_message(
                "auth.secret must be set explicitly when more than one node can run -- \
                 a Redis cache, event transport, or rate-limit backend is configured, and \
                 each node would otherwise generate its own secret, rejecting the other \
                 nodes' sessions and unable to read what they encrypted. Generate one with \
                 `openssl rand -hex 32`.",
            );
        }

        if self.database.backend == DatabaseBackend::Postgres {
            warn!(
                "auth.secret is unset on Postgres -- each node generates its own secret, so \
                 running a second node will break sessions and encrypted data; set \
                 auth.secret explicitly before scaling out"
            );
        }
    }

    /// Validate email/SMTP settings.
    pub(in crate::config) fn validate_email(&self, report: &mut ErrorReport) {
        if !self.email.smtp_host.is_empty() && self.email.smtp_port == 0 {
            report.push_message("email.smtp_port must be > 0 when smtp_host is configured");
        }
    }

    /// Validate logging settings.
    pub(in crate::config) fn validate_logging(&self, report: &mut ErrorReport) {
        if self.logging.file && self.logging.path.is_empty() {
            report.push_message("logging.path must not be empty when file logging is enabled");
        }

        if self.logging.file && self.logging.max_files == 0 {
            warn!("logging.max_files = 0 -- all rotated log files will be deleted on startup");
        }
    }

    /// Validate MCP settings.
    ///
    /// When `mcp.http = true`, enforces both presence and a minimum length
    /// on `mcp.api_key`. MCP operates with `overrideAccess = true` semantics
    /// (collection- and field-level ACLs are bypassed), so a weak transport
    /// key exposes the entire dataset -- a 32-byte floor keeps brute-force
    /// infeasible for realistic attacker budgets.
    pub(in crate::config) fn validate_mcp(&self, report: &mut ErrorReport) {
        // Checked before the HTTP-only gate: batching works on stdio too.
        if self.mcp.max_batch_members > MAX_MCP_BATCH_MEMBERS {
            report.push_message(format!(
                "mcp.max_batch_members is {} -- the ceiling is {}. A batch \
                 multiplies what one request can cost (each member can be a \
                 whole-collection `delete_many`), so an unbounded value \
                 undoes the cap's purpose. Use 0 to refuse batches entirely.",
                self.mcp.max_batch_members, MAX_MCP_BATCH_MEMBERS,
            ));
        }

        if !(self.mcp.enabled && self.mcp.http) {
            return;
        }

        let key_len = self.mcp.api_key.as_ref().len();

        if key_len == 0 {
            report.push_message(
                "mcp.http is enabled without an API key -- \
                 set mcp.api_key in crap.toml to secure the MCP HTTP endpoint",
            );
            return;
        }

        if key_len < MIN_MCP_API_KEY_LEN {
            report.push_message(format!(
                "mcp.api_key is too short ({key_len} chars) -- require at least \
                 {MIN_MCP_API_KEY_LEN} characters. MCP bypasses collection and field ACLs, so \
                 a short key risks exposing the entire dataset. Generate one with \
                 `openssl rand -hex 32` or `head -c 32 /dev/urandom | base64`."
            ));
        }
    }

    /// Validate live event streaming settings.
    pub(in crate::config) fn validate_live(&self, report: &mut ErrorReport) {
        if self.live.enabled && self.live.channel_capacity == 0 {
            report.push_message("live.channel_capacity must be > 0 when live events are enabled");
        }
    }

    /// Validate cache settings (advisory only).
    pub(in crate::config) fn validate_cache(&self) {
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
    use crate::{config::McpApiKey, core::JwtSecret};

    #[test]
    fn validate_token_expiry_zero_errors() {
        let mut config = CrapConfig::default();
        config.auth.token_expiry = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("token_expiry"));
    }

    #[test]
    fn validate_short_auth_secret_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new("short");
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
        config.mcp.api_key = McpApiKey::from("0123456789abcdef0123456789abcdef");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_mcp_http_with_short_api_key_errors() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        config.mcp.api_key = McpApiKey::from("secret-key-1234");
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
}
