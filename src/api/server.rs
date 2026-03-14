//! gRPC server startup and parameters.

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use tonic::transport::Server;

use crate::{
    config::CrapConfig,
    core::{
        JwtSecret, Registry,
        email::EmailRenderer,
        event::EventBus,
        rate_limit::{GrpcRateLimiter, LoginRateLimiter},
    },
    db::DbPool,
    hooks::HookRunner,
};

use super::{content, rate_limit, service};

/// Parameters for starting the gRPC API server.
pub struct GrpcStartParams {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub hook_runner: HookRunner,
    pub jwt_secret: JwtSecret,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub event_bus: Option<EventBus>,
}

impl GrpcStartParams {
    /// Create a builder for `GrpcStartParams`.
    pub fn builder() -> super::server_builder::GrpcStartParamsBuilder {
        super::server_builder::GrpcStartParamsBuilder::new()
    }
}

/// Start the gRPC server. Reflection is enabled by default but can be
/// disabled via `config.server.grpc_reflection`.
#[cfg(not(tarpaulin_include))]
pub async fn start(
    addr: &str,
    params: GrpcStartParams,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let addr = addr.parse()?;

    let email_renderer = Arc::new(EmailRenderer::new(&params.config_dir)?);
    let login_limiter = Arc::new(LoginRateLimiter::new(
        params.config.auth.max_login_attempts,
        params.config.auth.login_lockout_seconds,
    ));
    let forgot_password_limiter = Arc::new(LoginRateLimiter::new(
        params.config.auth.max_forgot_password_attempts,
        params.config.auth.forgot_password_window_seconds,
    ));

    let populate_cache_max_age = params.config.depth.populate_cache_max_age_secs;
    let grpc_rate_requests = params.config.server.grpc_rate_limit_requests;
    let grpc_rate_window = params.config.server.grpc_rate_limit_window;
    let grpc_reflection = params.config.server.grpc_reflection;
    let cors_layer = params.config.cors.build_layer();

    let content_service = service::ContentService::new(
        service::ContentServiceDeps::builder()
            .pool(params.pool)
            .registry(params.registry)
            .hook_runner(params.hook_runner)
            .jwt_secret(params.jwt_secret)
            .config(params.config)
            .config_dir(params.config_dir)
            .email_renderer(email_renderer)
            .event_bus(params.event_bus)
            .login_limiter(login_limiter)
            .forgot_password_limiter(forgot_password_limiter)
            .build(),
    );

    // Spawn periodic cache clear task for external DB mutation handling
    if populate_cache_max_age > 0
        && let Some(cache) = content_service.populate_cache_handle()
    {
        let interval_secs = populate_cache_max_age;
        let cache_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            interval.tick().await; // skip first immediate tick
            loop {
                tokio::select! {
                    _ = interval.tick() => cache.clear(),
                    _ = cache_shutdown.cancelled() => break,
                }
            }
        });
    }

    let grpc_limiter = Arc::new(GrpcRateLimiter::new(grpc_rate_requests, grpc_rate_window));
    let rate_limit_layer = rate_limit::GrpcRateLimitLayer::new(grpc_limiter);

    let content_svc = content::content_api_server::ContentApiServer::new(content_service);

    // gRPC health service (grpc.health.v1.Health)
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<content::content_api_server::ContentApiServer<service::ContentService>>()
        .await;

    let shutdown_signal = shutdown.cancelled_owned();

    let reflection_service = if grpc_reflection {
        Some(
            tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(content::FILE_DESCRIPTOR_SET)
                .build_v1()?,
        )
    } else {
        None
    };

    Server::builder()
        .layer(tower::util::option_layer(cors_layer))
        .layer(rate_limit_layer)
        .add_service(health_service)
        .add_optional_service(reflection_service)
        .add_service(content_svc)
        .serve_with_shutdown(addr, shutdown_signal)
        .await?;

    Ok(())
}
