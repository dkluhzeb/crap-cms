//! Model Context Protocol (MCP) server configuration.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::config::{McpApiKey, parsing::serde_filesize};

/// Which background-job tools the MCP surface exposes. Accepts `false`,
/// `"read"`, or `"all"` in `crap.toml` (the `false`-or-mode shape the auth
/// `mfa` setting uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpJobTools {
    /// No job tools at all (default).
    #[default]
    Off,
    /// Introspection only: `list_jobs`, `get_job_run`, `list_job_runs`.
    Read,
    /// Introspection plus `trigger_job`.
    All,
}

impl McpJobTools {
    /// Whether the read-only job tools are exposed. Also gates the `queue`
    /// argument on the MCP bulk tools — queueing without a way to poll the
    /// resulting `job_id` would be a dead end.
    #[must_use]
    pub fn reads(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether `trigger_job` is exposed.
    #[must_use]
    pub fn trigger(self) -> bool {
        matches!(self, Self::All)
    }

    /// The `crap.toml` spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "false",
            Self::Read => "read",
            Self::All => "all",
        }
    }
}

impl<'de> Deserialize<'de> for McpJobTools {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Str(String),
        }

        match Raw::deserialize(deserializer)? {
            Raw::Bool(false) => Ok(Self::Off),
            // `true` is ambiguous between the two enabled tiers — refuse it
            // rather than silently picking one.
            Raw::Bool(true) => Err(de::Error::custom(
                "job_tools = true is ambiguous — use \"read\" (introspection only) \
                 or \"all\" (also exposes trigger_job)",
            )),
            Raw::Str(s) => match s.as_str() {
                "off" | "false" => Ok(Self::Off),
                "read" => Ok(Self::Read),
                "all" => Ok(Self::All),
                other => Err(de::Error::custom(format!(
                    "unknown job_tools value '{other}' — expected false, \"read\", or \"all\""
                ))),
            },
        }
    }
}

impl Serialize for McpJobTools {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Off => serializer.serialize_bool(false),
            Self::Read => serializer.serialize_str("read"),
            Self::All => serializer.serialize_str("all"),
        }
    }
}

/// MCP (Model Context Protocol) server configuration.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Enable MCP server (default: false).
    pub enabled: bool,
    /// Enable HTTP transport on /mcp (default: false).
    pub http: bool,
    /// Enable config generation tools that can write files to disk (default: false).
    pub config_tools: bool,
    /// Background-job tools. `false` (default) exposes none; `"read"` adds
    /// `list_jobs` / `get_job_run` / `list_job_runs` (introspection and
    /// failure triage, and the polling half of queued bulk operations);
    /// `"all"` additionally exposes `trigger_job`, which queues **any**
    /// defined job. Gated because MCP runs with `override_access`, so a
    /// job's own `access` hook cannot restrict it — same rationale as
    /// `config_tools`. The `queue` argument on the bulk tools appears only
    /// from `"read"` up, so a client is never handed a `job_id` it has no
    /// tool to poll.
    #[serde(default)]
    pub job_tools: McpJobTools,
    /// API key for HTTP transport auth. **Required** when `http = true` -- the server
    /// will refuse to start without one. The HTTP handler also rejects all requests
    /// when the API key is empty as a defense-in-depth measure.
    pub api_key: McpApiKey,
    /// Whitelist of collection slugs to expose (empty = all).
    pub include_collections: Vec<String>,
    /// Blacklist of collection slugs to hide (takes precedence over include).
    pub exclude_collections: Vec<String>,
    /// Maximum HTTP request-body size for the `/mcp` endpoint, in bytes.
    /// Accepts integer bytes or a filesize string ("1MB", "16MB").
    /// Default: 1 MiB. Raise it when MCP clients push large payloads
    /// (bulk creates, `write_config_file` with big assets).
    #[serde(with = "serde_filesize")]
    pub http_max_body_bytes: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http: false,
            config_tools: false,
            job_tools: McpJobTools::Off,
            api_key: McpApiKey::default(),
            include_collections: Vec::new(),
            exclude_collections: Vec::new(),
            http_max_body_bytes: 1_048_576, // 1 MiB
        }
    }
}
