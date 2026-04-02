//! gRPC server startup and parameters.

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use tokio::select;
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
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub storage: crate::core::upload::SharedStorage,
    pub cache: crate::core::cache::SharedCache,
    pub token_provider: crate::core::auth::SharedTokenProvider,
    pub password_provider: crate::core::auth::SharedPasswordProvider,
    pub rate_limit_backend: crate::core::rate_limit::SharedRateLimitBackend,
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

    let cache_max_age = params.config.cache.max_age_secs;
    let grpc_rate_requests = params.config.server.grpc_rate_limit_requests;
    let grpc_rate_window = params.config.server.grpc_rate_limit_window;
    let grpc_reflection = params.config.server.grpc_reflection;
    let grpc_timeout = params.config.server.grpc_timeout;
    let grpc_max_msg = params.config.server.grpc_max_message_size as usize;
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
            .login_limiter(params.login_limiter)
            .ip_login_limiter(params.ip_login_limiter)
            .forgot_password_limiter(params.forgot_password_limiter)
            .ip_forgot_password_limiter(params.ip_forgot_password_limiter)
            .storage(params.storage)
            .cache(params.cache)
            .token_provider(params.token_provider)
            .password_provider(params.password_provider)
            .build(),
    );

    // Spawn periodic cache clear task for external DB mutation handling
    if cache_max_age > 0 && content_service.cache_handle().kind() != "none" {
        let cache = content_service.cache_handle();
        let interval_secs = cache_max_age;
        let cache_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            interval.tick().await; // skip first immediate tick
            loop {
                select! {
                    _ = interval.tick() => {
                        if let Err(e) = cache.clear() {
                            tracing::warn!("Periodic cache clear failed: {:#}", e);
                        }
                    },
                    _ = cache_shutdown.cancelled() => break,
                }
            }
        });
    }

    let grpc_limiter = Arc::new(GrpcRateLimiter::with_backend(
        params.rate_limit_backend,
        grpc_rate_requests,
        grpc_rate_window,
    ));
    let rate_limit_layer = rate_limit::GrpcRateLimitLayer::new(grpc_limiter);

    let content_svc = content::content_api_server::ContentApiServer::new(content_service)
        .max_decoding_message_size(grpc_max_msg)
        .max_encoding_message_size(grpc_max_msg);

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

    let mut builder = Server::builder()
        .layer(tower::util::option_layer(cors_layer))
        .layer(rate_limit_layer);

    // Apply gRPC timeout if configured (applies to all RPCs including Subscribe)
    if let Some(timeout_secs) = grpc_timeout {
        builder = builder.timeout(std::time::Duration::from_secs(timeout_secs));
    }

    builder
        .add_service(health_service)
        .add_optional_service(reflection_service)
        .add_service(content_svc)
        .serve_with_shutdown(addr, shutdown_signal)
        .await?;

    Ok(())
}
