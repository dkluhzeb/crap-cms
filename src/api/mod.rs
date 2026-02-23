//! gRPC API server (Tonic) implementing the ContentAPI service.

mod service;

use anyhow::Result;
use tonic::transport::Server;

use crate::core::SharedRegistry;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

pub mod content {
    tonic::include_proto!("crap");

    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("content_descriptor");
}

/// Start the gRPC server with reflection enabled.
pub async fn start_server(
    addr: &str,
    pool: DbPool,
    registry: SharedRegistry,
    hook_runner: HookRunner,
    jwt_secret: String,
    depth_config: &crate::config::DepthConfig,
    config: &crate::config::CrapConfig,
    config_dir: &std::path::Path,
) -> Result<()> {
    let addr = addr.parse()?;

    let email_renderer = std::sync::Arc::new(
        crate::core::email::EmailRenderer::new(config_dir)?
    );
    let content_service = service::ContentService::new(
        pool, registry, hook_runner, jwt_secret, depth_config,
        config.email.clone(), email_renderer, config.server.clone(),
    );

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(content::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    Server::builder()
        .add_service(reflection_service)
        .add_service(content::content_api_server::ContentApiServer::new(content_service))
        .serve(addr)
        .await?;

    Ok(())
}
