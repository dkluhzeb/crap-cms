//! Server, database, and admin configuration structs.

use serde::{Deserialize, Serialize};

use super::parsing::{serde_duration, serde_duration_ms, serde_duration_option, serde_filesize};

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
#[serde(default)]
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
    /// Trust X-Forwarded-For header for client IP extraction (admin HTTP server only).
    /// Enable when running behind a reverse proxy (nginx, Caddy, etc.).
    /// When false (default), the TCP socket address is used — XFF is ignored.
    /// Does not affect gRPC, which always uses the TCP peer address.
    pub trust_proxy: bool,
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
            public_url: None,
            request_timeout: None,
            grpc_timeout: None,
            grpc_max_message_size: default_grpc_max_message_size(),
        }
    }
}

/// SQLite database path and pool configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// Path to the SQLite database file.
    pub path: String,
    /// Maximum number of connections in the pool. Default: 64.
    pub pool_max_size: u32,
    /// SQLite busy timeout in milliseconds. Default: 30000 (30s).
    /// Accepts integer milliseconds or human-readable string ("30s", "1m").
    #[serde(with = "serde_duration_ms")]
    pub busy_timeout: u64,
    /// Pool connection timeout in seconds. Default: 5.
    /// Accepts integer seconds or human-readable string ("5s", "10s").
    #[serde(with = "serde_duration")]
    pub connection_timeout: u64,
    /// SQLite page cache size in KB. Negative = KB, positive = pages. Default: 16384 (16MB).
    /// Higher values improve read performance for large datasets.
    #[serde(default = "default_cache_size")]
    pub cache_size: i64,
    /// SQLite memory-mapped I/O size in bytes. Default: 268435456 (256MB).
    /// Set to 0 to disable. Improves read throughput for databases smaller than this value.
    #[serde(default = "default_mmap_size")]
    pub mmap_size: u64,
    /// SQLite WAL auto-checkpoint threshold in pages. Default: 1000.
    /// Lower values keep the WAL file smaller; higher values reduce checkpoint frequency.
    #[serde(default = "default_wal_autocheckpoint")]
    pub wal_autocheckpoint: u32,
}

fn default_cache_size() -> i64 {
    -16384
}

fn default_mmap_size() -> u64 {
    268_435_456
}

fn default_wal_autocheckpoint() -> u32 {
    1000
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: "data/crap.db".to_string(),
            pool_max_size: 64,
            busy_timeout: 30000,
            connection_timeout: 5,
            cache_size: default_cache_size(),
            mmap_size: default_mmap_size(),
            wal_autocheckpoint: default_wal_autocheckpoint(),
        }
    }
}

/// Content-Security-Policy configuration for the admin UI.
///
/// Each field is a list of sources for the corresponding CSP directive.
/// Theme developers can extend these lists to allow external resources
/// (CDNs, fonts, analytics, etc.).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CspConfig {
    /// Enable CSP header. Default: true. Set to false to disable entirely.
    pub enabled: bool,
    /// `default-src` directive — fallback for unspecified directives.
    pub default_src: Vec<String>,
    /// `script-src` directive — allowed script sources.
    pub script_src: Vec<String>,
    /// `style-src` directive — allowed stylesheet sources.
    pub style_src: Vec<String>,
    /// `font-src` directive — allowed font sources.
    pub font_src: Vec<String>,
    /// `img-src` directive — allowed image sources.
    pub img_src: Vec<String>,
    /// `connect-src` directive — allowed fetch/XHR/WebSocket targets.
    pub connect_src: Vec<String>,
    /// `frame-ancestors` directive — who can embed this page. Replaces X-Frame-Options.
    pub frame_ancestors: Vec<String>,
    /// `form-action` directive — allowed form submission targets.
    pub form_action: Vec<String>,
    /// `base-uri` directive — allowed `<base>` tag URLs.
    pub base_uri: Vec<String>,
}

impl Default for CspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_src: vec!["'self'".into()],
            script_src: vec![
                "'self'".into(),
                "'unsafe-inline'".into(),
                "https://unpkg.com".into(),
            ],
            style_src: vec![
                "'self'".into(),
                "'unsafe-inline'".into(),
                "https://fonts.googleapis.com".into(),
            ],
            font_src: vec!["'self'".into(), "https://fonts.gstatic.com".into()],
            img_src: vec!["'self'".into(), "data:".into()],
            connect_src: vec!["'self'".into()],
            frame_ancestors: vec!["'none'".into()],
            form_action: vec!["'self'".into()],
            base_uri: vec!["'self'".into()],
        }
    }
}

impl CspConfig {
    /// Build the CSP header value string from configured directives.
    /// Returns `None` if CSP is disabled.
    pub fn build_header_value(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }

        let mut directives = Vec::new();

        let pairs: &[(&str, &[String])] = &[
            ("default-src", &self.default_src),
            ("script-src", &self.script_src),
            ("style-src", &self.style_src),
            ("font-src", &self.font_src),
            ("img-src", &self.img_src),
            ("connect-src", &self.connect_src),
            ("frame-ancestors", &self.frame_ancestors),
            ("form-action", &self.form_action),
            ("base-uri", &self.base_uri),
        ];

        for (name, sources) in pairs {
            if !sources.is_empty() {
                directives.push(format!("{} {}", name, sources.join(" ")));
            }
        }

        if directives.is_empty() {
            return None;
        }

        Some(directives.join("; "))
    }
}

/// Admin UI behavior settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AdminConfig {
    /// Enable development mode (e.g., more verbose errors).
    pub dev_mode: bool,
    /// When true (default), block admin panel if no auth collection exists.
    /// Set to false for open dev mode with no authentication.
    pub require_auth: bool,
    /// Optional Lua function ref that gates admin panel access.
    /// Checked after successful authentication. None = any authenticated user.
    pub access: Option<String>,
    /// Content-Security-Policy header configuration.
    pub csp: CspConfig,
    /// Default IANA timezone for date fields with `timezone = true` that don't
    /// specify their own `default_timezone`. Empty string means no pre-selection.
    #[serde(default)]
    pub default_timezone: String,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            dev_mode: false,
            require_auth: true,
            access: None,
            csp: CspConfig::default(),
            default_timezone: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::CrapConfig;

    use super::*;

    #[test]
    fn admin_config_defaults() {
        let admin = AdminConfig::default();
        assert!(!admin.dev_mode);
        assert!(admin.require_auth);
        assert!(admin.access.is_none());
    }

    #[test]
    fn admin_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin]\ndev_mode = true\nrequire_auth = false\naccess = \"access.admin_panel\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(!config.admin.require_auth);
        assert_eq!(config.admin.access, Some("access.admin_panel".to_string()));
    }

    #[test]
    fn admin_config_partial_toml_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "[admin]\ndev_mode = true\n").unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(config.admin.require_auth); // default
        assert!(config.admin.access.is_none()); // default
    }

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
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[server]\ntrust_proxy = true\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.server.trust_proxy);
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

    #[test]
    fn csp_config_defaults_produce_valid_header() {
        let csp = CspConfig::default();
        let header = csp.build_header_value();
        assert!(header.is_some());
        let h = header.unwrap();
        assert!(h.contains("default-src 'self'"));
        assert!(h.contains("script-src 'self' 'unsafe-inline' https://unpkg.com"));
        assert!(h.contains("style-src 'self' 'unsafe-inline' https://fonts.googleapis.com"));
        assert!(h.contains("font-src 'self' https://fonts.gstatic.com"));
        assert!(h.contains("img-src 'self' data:"));
        assert!(h.contains("connect-src 'self'"));
        assert!(h.contains("frame-ancestors 'none'"));
        assert!(h.contains("form-action 'self'"));
        assert!(h.contains("base-uri 'self'"));
    }

    #[test]
    fn csp_config_disabled_returns_none() {
        let csp = CspConfig {
            enabled: false,
            ..CspConfig::default()
        };
        assert!(csp.build_header_value().is_none());
    }

    #[test]
    fn csp_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin.csp]\nenabled = true\nscript_src = [\"'self'\", \"https://cdn.example.com\"]\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.csp.enabled);
        assert_eq!(
            config.admin.csp.script_src,
            vec!["'self'", "https://cdn.example.com"]
        );
        // Other directives keep defaults
        assert!(config.admin.csp.style_src.contains(&"'self'".to_string()));
    }

    #[test]
    fn csp_config_disabled_via_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[admin.csp]\nenabled = false\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(!config.admin.csp.enabled);
        assert!(config.admin.csp.build_header_value().is_none());
    }

    #[test]
    fn csp_config_empty_directive_omitted() {
        let csp = CspConfig {
            enabled: true,
            default_src: vec!["'self'".into()],
            script_src: vec![],
            style_src: vec![],
            font_src: vec![],
            img_src: vec![],
            connect_src: vec![],
            frame_ancestors: vec![],
            form_action: vec![],
            base_uri: vec![],
        };
        let header = csp.build_header_value().unwrap();
        assert_eq!(header, "default-src 'self'");
        assert!(!header.contains("script-src"));
    }

    #[test]
    fn database_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[database]\npool_max_size = 32\nbusy_timeout = 60000\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.pool_max_size, 32);
        assert_eq!(config.database.busy_timeout, 60000);
    }
}
