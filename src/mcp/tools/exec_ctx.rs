//! Shared execution context for MCP tool functions.
//!
//! Bundles the process-stable [`AppInfra`] every CRUD-style tool needs, plus the
//! resolved config and an audit label. Lets each `exec_*` fn take a flat
//! `(args, slug, ctx)` signature instead of 7-9 positional params.
//!
//! `args` and `slug` stay separate because they're per-call inputs; the infra
//! and config here are stable across calls within a single MCP server lifetime.

use std::sync::Arc;

use crate::{config::CrapConfig, service::AppInfra};

pub(in crate::mcp) struct ToolExecCtx<'a> {
    /// Process-stable infrastructure — pool, registry, hook runner, caches,
    /// transports, storage. MCP uses the "core" subset: it runs
    /// `override_access` with transport-level auth (process access for stdio,
    /// API key for HTTP), so the auth / email / populate-singleflight fields of
    /// [`AppInfra`] are present but unused here. Locale and password policy are
    /// read from [`Self::config`] rather than the pre-extracted infra fields.
    ///
    /// Held as an owned `Arc` (cheap clone) so a `ToolExecCtx` doesn't have to
    /// borrow a separately-kept `AppInfra` — the HTTP transport clones the
    /// server's shared bundle, the stdio/test transports build their own.
    pub infra: Arc<AppInfra>,
    pub config: &'a CrapConfig,
    /// Audit identifier for the current call. The literal client
    /// name from the MCP `initialize` handshake when known (stdio
    /// after init); otherwise the transport-level fallback wrapped
    /// in parens — `(stdio)`, `(http)`, `(test)` — so a real client
    /// named "stdio" still reads distinctly. Logged on every
    /// mutation so we can answer "what did this client touch"
    /// without per-line plumbing.
    pub client_label: &'a str,
}
