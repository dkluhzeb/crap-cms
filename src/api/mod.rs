//! gRPC API server (Tonic) implementing the ContentAPI service.

pub mod service;
pub mod upload;

use anyhow::Result;
use tonic::transport::Server;

use crate::core::SharedRegistry;
use crate::core::event::EventBus;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

pub mod content {
    tonic::include_proto!("crap");

    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("content_descriptor");
}

/// Start the gRPC server with reflection enabled.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub async fn start_server(
    addr: &str,
    pool: DbPool,
    registry: SharedRegistry,
    hook_runner: HookRunner,
    jwt_secret: String,
    depth_config: &crate::config::DepthConfig,
    config: &crate::config::CrapConfig,
    config_dir: &std::path::Path,
    event_bus: Option<EventBus>,
) -> Result<()> {
    let addr = addr.parse()?;

    let email_renderer = std::sync::Arc::new(
        crate::core::email::EmailRenderer::new(config_dir)?
    );
    let login_limiter = std::sync::Arc::new(
        crate::core::rate_limit::LoginRateLimiter::new(
            config.auth.max_login_attempts,
            config.auth.login_lockout_seconds,
        )
    );
    let content_service = service::ContentService::new(
        pool, registry, hook_runner, jwt_secret, depth_config,
        config.email.clone(), email_renderer, config.server.clone(),
        event_bus, config.locale.clone(), config_dir.to_path_buf(),
        login_limiter,
    );

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(content::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    if let Some(cors) = config.cors.build_layer() {
        Server::builder()
            .layer(cors)
            .add_service(reflection_service)
            .add_service(content::content_api_server::ContentApiServer::new(content_service))
            .serve(addr)
            .await?;
    } else {
        Server::builder()
            .add_service(reflection_service)
            .add_service(content::content_api_server::ContentApiServer::new(content_service))
            .serve(addr)
            .await?;
    }

    Ok(())
}
