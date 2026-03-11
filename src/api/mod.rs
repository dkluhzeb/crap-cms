//! gRPC API server (Tonic) implementing the ContentAPI service.

pub mod rate_limit;
pub mod service;
pub mod upload;

use anyhow::Result;
use tonic::transport::Server;

use crate::core::Registry;
use crate::core::event::EventBus;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

/// Generated gRPC content service types.
pub mod content {
    tonic::include_proto!("crap");

    /// File descriptor set for gRPC reflection.
    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("content_descriptor");
}

/// Start the gRPC server. Reflection is enabled by default but can be
/// disabled via `config.server.grpc_reflection`.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub async fn start_server(
    addr: &str,
    pool: DbPool,
    registry: std::sync::Arc<Registry>,
    hook_runner: HookRunner,
    jwt_secret: String,
    depth_config: &crate::config::DepthConfig,
    config: &crate::config::CrapConfig,
    config_dir: &std::path::Path,
    event_bus: Option<EventBus>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let addr = addr.parse()?;

    let email_renderer = std::sync::Arc::new(crate::core::email::EmailRenderer::new(config_dir)?);
    let login_limiter = std::sync::Arc::new(crate::core::rate_limit::LoginRateLimiter::new(
        config.auth.max_login_attempts,
        config.auth.login_lockout_seconds,
    ));
    let forgot_password_limiter =
        std::sync::Arc::new(crate::core::rate_limit::LoginRateLimiter::new(
            config.auth.max_forgot_password_attempts,
            config.auth.forgot_password_window_seconds,
        ));
    let content_service = service::ContentService::new(
        pool,
        registry,
        hook_runner,
        jwt_secret,
        depth_config,
        &config.pagination,
        config.email.clone(),
        email_renderer,
        config.server.clone(),
        event_bus,
        config.locale.clone(),
        config_dir.to_path_buf(),
        login_limiter,
        config.auth.reset_token_expiry,
        config.auth.password_policy.clone(),
        forgot_password_limiter,
    );

    // Spawn periodic cache clear task for external DB mutation handling
    if depth_config.populate_cache_max_age_secs > 0 {
        if let Some(cache) = content_service.populate_cache_handle() {
            let interval_secs = depth_config.populate_cache_max_age_secs;
            tokio::spawn(async move {
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_secs(interval_secs));
                interval.tick().await; // skip first immediate tick
                loop {
                    interval.tick().await;
                    cache.clear();
                }
            });
        }
    }

    let grpc_limiter = std::sync::Arc::new(crate::core::rate_limit::GrpcRateLimiter::new(
        config.server.grpc_rate_limit_requests,
        config.server.grpc_rate_limit_window,
    ));
    let rate_limit_layer = rate_limit::GrpcRateLimitLayer::new(grpc_limiter);

    let content_svc = content::content_api_server::ContentApiServer::new(content_service);

    // gRPC health service (grpc.health.v1.Health)
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<content::content_api_server::ContentApiServer<service::ContentService>>()
        .await;

    let shutdown_signal = shutdown.cancelled_owned();

    if config.server.grpc_reflection {
        let reflection_service = tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(content::FILE_DESCRIPTOR_SET)
            .build_v1()?;

        if let Some(cors) = config.cors.build_layer() {
            Server::builder()
                .layer(cors)
                .layer(rate_limit_layer)
                .add_service(health_service)
                .add_service(reflection_service)
                .add_service(content_svc)
                .serve_with_shutdown(addr, shutdown_signal)
                .await?;
        } else {
            Server::builder()
                .layer(rate_limit_layer)
                .add_service(health_service)
                .add_service(reflection_service)
                .add_service(content_svc)
                .serve_with_shutdown(addr, shutdown_signal)
                .await?;
        }
    } else if let Some(cors) = config.cors.build_layer() {
        Server::builder()
            .layer(cors)
            .layer(rate_limit_layer)
            .add_service(health_service)
            .add_service(content_svc)
            .serve_with_shutdown(addr, shutdown_signal)
            .await?;
    } else {
        Server::builder()
            .layer(rate_limit_layer)
            .add_service(health_service)
            .add_service(content_svc)
            .serve_with_shutdown(addr, shutdown_signal)
            .await?;
    }

    Ok(())
}
