//! Bind ports, host, compression, gRPC, and reverse-proxy trust settings.

use serde::{Deserialize, Serialize};

use crate::config::parsing::{serde_duration, serde_duration_option, serde_filesize};

/// Response compression mode for the admin HTTP server.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompressionMode {
    /// Disable compression (default).
    #[default]
    Off,
    /// Enable Gzip compression.
    Gzip,
    /// Enable Brotli compression.
    Br,
    /// Enable all supported compression modes.
    All,
}

/// Admin UI and gRPC server bind settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Port for the admin UI HTTP server. Default: 3000.
    pub admin_port: u16,
    /// Port for the gRPC API server. Default: 50051.
    pub grpc_port: u16,
    /// Host interface to bind to. Default: "0.0.0.0".
    pub host: String,
    /// Enable response compression. Default: off (most deployments use a reverse proxy).
    /// Options: "off", "gzip", "br", "all".
    pub compression: CompressionMode,
    /// Enable gRPC server reflection (allows clients to discover services).
    /// Default: false. Enable during development to allow clients to discover services.
    pub grpc_reflection: bool,
    /// Per-IP gRPC rate limit: max requests per window. 0 = disabled (default).
    pub grpc_rate_limit_requests: u32,
    /// Sliding window duration in seconds for gRPC rate limiting.
    #[serde(default = "default_grpc_rate_limit_window", with = "serde_duration")]
    pub grpc_rate_limit_window: u64,
    /// Enable HTTP/2 cleartext (h2c) for the admin server.
    /// Allows reverse proxies to speak HTTP/2 to the backend without TLS.
    /// Browsers that don't support h2c fall back to HTTP/1.1 on the same port.
    /// Default: false.
    pub h2c: bool,
    /// Trust `X-Forwarded-For` for client IP extraction (admin HTTP only).
    /// Enable when running behind a reverse proxy (nginx, Caddy, etc.).
    /// When false (default), the TCP socket address is used -- XFF is ignored.
    /// Does not affect gRPC, which always uses the TCP peer address.
    ///
    /// **Security:** if this is `true` but `trusted_proxies` is empty, any
    /// client can spoof `X-Forwarded-For` and rotate per-IP rate limits
    /// (login, password reset). Always pair `trust_proxy = true` with a
    /// `trusted_proxies` allowlist containing the reverse proxy's IP or
    /// CIDR range. Startup emits a warning when this pairing is missing.
    pub trust_proxy: bool,
    /// IP addresses or CIDR ranges allowed to set `X-Forwarded-For`. When
    /// non-empty and `trust_proxy = true`, the XFF header is honored only
    /// if the request's direct peer address is in this list; otherwise
    /// the TCP socket address is used. Accepts both IPv4 and IPv6 in
    /// either bare (`10.0.0.1`) or CIDR (`10.0.0.0/8`, `::1/128`) form.
    ///
    /// Empty (default) preserves the pre-hardening behaviour: when
    /// `trust_proxy` is `true`, XFF is trusted unconditionally. A warning
    /// is logged at startup for this combination.
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// Public-facing base URL (e.g. "https://cms.example.com"). Used for password reset
    /// emails and other external links. When not set, falls back to http://{host}:{admin_port}.
    pub public_url: Option<String>,
    /// HTTP request timeout for the admin server in seconds. None = no timeout (default).
    /// Applies to all admin HTTP requests. SSE streams are exempt (handled by shutdown).
    /// Accepts integer seconds or human-readable string ("30s", "5m").
    #[serde(default, with = "serde_duration_option")]
    pub request_timeout: Option<u64>,
    /// gRPC request timeout in seconds. None = no timeout (default).
    /// Applies to all gRPC RPCs including Subscribe streams.
    /// Accepts integer seconds or human-readable string ("30s", "5m").
    #[serde(default, with = "serde_duration_option")]
    pub grpc_timeout: Option<u64>,
    /// Max gRPC message size in bytes (applies to both send and receive).
    /// Default: 16MB. Tonic's built-in default is only 4MB, which can be exceeded
    /// by large Find responses (1000 docs with deep population).
    /// Accepts integer bytes or human-readable string ("16MB", "32MB").
    #[serde(default = "default_grpc_max_message_size", with = "serde_filesize")]
    pub grpc_max_message_size: u64,
}

fn default_grpc_rate_limit_window() -> u64 {
    60
}

fn default_grpc_max_message_size() -> u64 {
    16 * 1024 * 1024 // 16MB
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            admin_port: 3000,
            grpc_port: 50051,
            host: "0.0.0.0".to_string(),
            compression: CompressionMode::Off,
            grpc_reflection: false,
            grpc_rate_limit_requests: 0,
            grpc_rate_limit_window: 60,
            h2c: false,
            trust_proxy: false,
            trusted_proxies: Vec::new(),
            public_url: None,
            request_timeout: None,
            grpc_timeout: None,
            grpc_max_message_size: default_grpc_max_message_size(),
        }
    }
}

impl ServerConfig {
    /// Return the public-facing base URL for generated links (password reset emails, etc.).
    ///
    /// Uses `public_url` if set, otherwise falls back to `http://{host}:{admin_port}`.
    /// Special-cases `0.0.0.0` -> `localhost` since `0.0.0.0` is a bind address, not reachable.
    pub fn base_url(&self) -> String {
        if let Some(ref url) = self.public_url {
            url.trim_end_matches('/').to_string()
        } else if self.host == "0.0.0.0" {
            format!("http://localhost:{}", self.admin_port)
        } else {
            format!("http://{}:{}", self.host, self.admin_port)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::CrapConfig;

    use super::*;

    #[test]
    fn server_config_h2c_defaults_to_false() {
        let server = ServerConfig::default();
        assert!(!server.h2c);
    }

    #[test]
    fn server_config_h2c_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "[server]\nh2c = true\n").unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.server.h2c);
    }

    #[test]
    fn server_config_h2c_omitted_uses_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 8080\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(!config.server.h2c);
    }

    #[test]
    fn server_config_trust_proxy_defaults_to_false() {
        let server = ServerConfig::default();
        assert!(!server.trust_proxy);
    }

    #[test]
    fn server_config_trust_proxy_from_toml() {
        // `trust_proxy = true` must be paired with `trusted_proxies` -- the
        // allowlist is now required at startup. Use a real-looking CIDR
        // here so validation passes.
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\ntrust_proxy = true\ntrusted_proxies = [\"10.0.0.0/8\"]\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.server.trust_proxy);
        assert_eq!(config.server.trusted_proxies, vec!["10.0.0.0/8"]);
    }

    #[test]
    fn server_config_trust_proxy_omitted_uses_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 8080\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(!config.server.trust_proxy);
    }

    #[test]
    fn server_config_request_timeout_defaults_to_none() {
        let server = ServerConfig::default();
        assert!(server.request_timeout.is_none());
        assert!(server.grpc_timeout.is_none());
    }

    #[test]
    fn server_config_request_timeout_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nrequest_timeout = 30\ngrpc_timeout = \"60s\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.request_timeout, Some(30));
        assert_eq!(config.server.grpc_timeout, Some(60));
    }

    #[test]
    fn server_config_request_timeout_human_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nrequest_timeout = \"5m\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.request_timeout, Some(300));
    }

    #[test]
    fn server_config_grpc_max_message_size_defaults_to_16mb() {
        let server = ServerConfig::default();
        assert_eq!(server.grpc_max_message_size, 16 * 1024 * 1024);
    }

    #[test]
    fn server_config_grpc_max_message_size_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\ngrpc_max_message_size = \"32MB\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.grpc_max_message_size, 32 * 1024 * 1024);
    }

    #[test]
    fn server_config_grpc_max_message_size_integer_bytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\ngrpc_max_message_size = 8388608\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.grpc_max_message_size, 8 * 1024 * 1024);
    }
}
