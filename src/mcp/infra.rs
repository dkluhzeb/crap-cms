//! Assemble a standalone [`AppInfra`] for MCP transports that build their own
//! process state — the stdio command and tests — rather than sharing the
//! boot-time bundle the way the HTTP transport does.

use std::{path::Path, sync::Arc};

use anyhow::Result;

use crate::{
    config::CrapConfig,
    core::{
        Registry, SharedStorage, auth::JwtTokenProvider, cache::create_cache, email::EmailRenderer,
        event::InProcessInvalidationBus,
    },
    db::{DbPool, Singleflight},
    hooks::HookRunner,
    service::{AppInfra, EmailContext},
};

/// Build a full [`AppInfra`] from MCP's core dependencies plus config.
///
/// MCP only uses the "core" subset (pool / registry / hook runner / cache /
/// storage / transports), so the auth and email fields are filled with
/// real-but-unused values: an ephemeral JWT provider (MCP runs `override_access`
/// with transport-level auth and never issues or validates tokens) and the
/// config's email context. The live-update event transport is off — a standalone
/// MCP process has no subscribers.
pub(crate) fn standalone_infra(
    pool: DbPool,
    registry: Arc<Registry>,
    hook_runner: HookRunner,
    storage: SharedStorage,
    config: &CrapConfig,
    config_dir: &Path,
) -> Result<Arc<AppInfra>> {
    let email = EmailContext {
        email_config: config.email.clone(),
        email_renderer: Arc::new(EmailRenderer::new(config_dir)?),
        server_config: config.server.clone(),
        email_max_attempts: config.jobs.system_email_max_attempts(),
    };

    Ok(Arc::new(
        AppInfra::builder()
            .pool(pool)
            .registry(registry)
            .hook_runner(hook_runner)
            .cache(create_cache(&config.cache)?)
            .storage(storage)
            .event_transport(None)
            .invalidation_transport(Arc::new(InProcessInvalidationBus::new()))
            // MCP never touches this provider; a fixed placeholder secret is fine.
            .token_provider(Arc::new(JwtTokenProvider::new(
                "mcp-standalone-unused-token-provider",
            )))
            .email(email)
            .locale_config(config.locale.clone())
            .password_policy(config.auth.password_policy.clone())
            .populate_singleflight(Arc::new(Singleflight::new()))
            .build(),
    ))
}
