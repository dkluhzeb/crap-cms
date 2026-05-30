//! Shared execution context for MCP tool functions.
//!
//! Bundles the deps every CRUD-style tool needs: registry, pool,
//! hook runner, config, and the optional event / invalidation /
//! cache attachments. Lets each `exec_*` fn take a flat
//! `(args, slug, ctx)` signature instead of 7-9 positional params.
//!
//! `args` and `slug` stay separate because they're per-call inputs;
//! everything in here is stable across calls within a single MCP
//! server lifetime.

use std::sync::Arc;

use crate::{
    config::CrapConfig,
    core::{
        Registry, SharedCache, SharedEventTransport, SharedInvalidationTransport, SharedStorage,
    },
    db::DbPool,
    hooks::HookRunner,
};

pub(in crate::mcp) struct ToolExecCtx<'a> {
    pub registry: &'a Arc<Registry>,
    pub pool: &'a DbPool,
    pub runner: &'a HookRunner,
    pub config: &'a CrapConfig,
    pub event_transport: Option<SharedEventTransport>,
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    pub cache: Option<SharedCache>,
    /// Storage backend for deleting uploaded files on hard-delete.
    /// `None` = files are left in place (tests without an upload backend).
    pub storage: Option<SharedStorage>,
    /// Audit identifier for the current call. The literal client
    /// name from the MCP `initialize` handshake when known (stdio
    /// after init); otherwise the transport-level fallback wrapped
    /// in parens — `(stdio)`, `(http)`, `(test)` — so a real client
    /// named "stdio" still reads distinctly. Logged on every
    /// mutation so we can answer "what did this client touch"
    /// without per-line plumbing.
    pub client_label: &'a str,
}
