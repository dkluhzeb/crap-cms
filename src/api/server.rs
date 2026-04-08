//! gRPC server startup and parameters.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use tokio::{select, spawn, time::interval};
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tracing::warn;

use crate::{
    api::{
        content::{FILE_DESCRIPTOR_SET, content_api_server::ContentApiServer},
        handlers::{ContentService, ContentServiceDeps},
        rate_limit::GrpcRateLimitLayer,
        server_builder::GrpcStartParamsBuilder,
    },
    config::CrapConfig,
    core::{
        JwtSecret, Registry,
        auth::{SharedPasswordProvider, SharedTokenProvider},
        cache::SharedCache,
        email::EmailRenderer,
        event::EventBus,
        rate_limit::{GrpcRateLimiter, LoginRateLimiter, SharedRateLimitBackend},
        upload::SharedStorage,
    },
    db::DbPool,
    hooks::HookRunner,
};

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
    pub storage: SharedStorage,
    pub cache: SharedCache,
    pub token_provider: SharedTokenProvider,
    pub password_provider: SharedPasswordProvider,
    pub rate_limit_backend: SharedRateLimitBackend,
}

impl GrpcStartParams {
    /// Create a builder for `GrpcStartParams`.
    pub fn builder() -> GrpcStartParamsBuilder {
        GrpcStartParamsBuilder::new()
    }
}

/// Start the gRPC server. Reflection is enabled by default but can be
/// disabled via `config.server.grpc_reflection`.
#[cfg(not(tarpaulin_include))]
pub async fn start(addr: &str, params: GrpcStartParams, shutdown: CancellationToken) -> Result<()> {
    let addr = addr.parse()?;

    let email_renderer = Arc::new(EmailRenderer::new(&params.config_dir)?);

    let cache_max_age = params.config.cache.max_age_secs;
    let grpc_rate_requests = params.config.server.grpc_rate_limit_requests;
    let grpc_rate_window = params.config.server.grpc_rate_limit_window;
    let grpc_reflection = params.config.server.grpc_reflection;
    let grpc_timeout = params.config.server.grpc_timeout;
    let grpc_max_msg = params.config.server.grpc_max_message_size as usize;
    let cors_layer = params.config.cors.build_layer();

    let content_service = ContentService::new(
        ContentServiceDeps::builder()
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

    if cache_max_age > 0 && content_service.cache_handle().kind() != "none" {
        spawn_periodic_cache_clear(
            content_service.cache_handle(),
            cache_max_age,
            shutdown.clone(),
        );
    }

    let grpc_limiter = Arc::new(GrpcRateLimiter::with_backend(
        params.rate_limit_backend,
        grpc_rate_requests,
        grpc_rate_window,
    ));
    let rate_limit_layer = GrpcRateLimitLayer::new(grpc_limiter);

    let content_svc = ContentApiServer::new(content_service)
        .max_decoding_message_size(grpc_max_msg)
        .max_encoding_message_size(grpc_max_msg);

    // gRPC health service (grpc.health.v1.Health)
    let (health_reporter, health_service) = tonic_health::server::health_reporter();

    health_reporter
        .set_serving::<ContentApiServer<ContentService>>()
        .await;

    let shutdown_signal = shutdown.cancelled_owned();

    let reflection_service = if grpc_reflection {
        Some(
            tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
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
        builder = builder.timeout(Duration::from_secs(timeout_secs));
    }

    builder
        .add_service(health_service)
        .add_optional_service(reflection_service)
        .add_service(content_svc)
        .serve_with_shutdown(addr, shutdown_signal)
        .await?;

    Ok(())
}

/// Spawn a background task that periodically clears the cache.
/// Handles external DB mutations that bypass the API's cache invalidation.
fn spawn_periodic_cache_clear(
    cache: crate::core::cache::SharedCache,
    interval_secs: u64,
    shutdown: CancellationToken,
) {
    spawn(async move {
        let mut tick = interval(Duration::from_secs(interval_secs));

        tick.tick().await; // skip first immediate tick

        loop {
            select! {
                _ = tick.tick() => {
                    if let Err(e) = cache.clear() {
                        warn!("Periodic cache clear failed: {:#}", e);
                    }
                },
                _ = shutdown.cancelled() => break,
            }
        }
    });
}
