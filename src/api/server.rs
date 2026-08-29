//! gRPC server startup and parameters.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use tokio::{select, spawn, time::interval};
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tonic_health::server::health_reporter;
use tracing::warn;

use crate::{
    api::{
        content::{FILE_DESCRIPTOR_SET, content_api_server::ContentApiServer},
        handlers::{ContentService, ContentServiceDeps},
        rate_limit::GrpcRateLimitLayer,
    },
    config::CrapConfig,
    core::{
        SharedCache, SharedPasswordProvider, SharedRateLimitBackend,
        rate_limit::{GrpcRateLimiter, LoginRateLimiter},
    },
    service::AppInfra,
};

/// Parameters for starting the gRPC API server.
///
/// All process-stable infrastructure (pool, registry, hook runner, caches,
/// transports, providers) lives in [`AppInfra`]; only the per-surface bits
/// (config, rate limiters, password provider, rate-limit backend) sit alongside
/// it.
pub struct GrpcStartParams {
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub mfa_limiter: Arc<LoginRateLimiter>,
    pub ip_mfa_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub password_provider: SharedPasswordProvider,
    pub rate_limit_backend: SharedRateLimitBackend,
    /// Process-stable infrastructure bundle, assembled once at boot and shared
    /// across surfaces. The service threads it into every `ServiceContext` via
    /// `.infra(&self.infra)`.
    pub infra: Arc<AppInfra>,
}

impl GrpcStartParams {
    /// Create a builder for `GrpcStartParams`.
    #[must_use]
    pub fn builder() -> GrpcStartParamsBuilder {
        GrpcStartParamsBuilder::new()
    }
}

/// Builder for [`GrpcStartParams`]. Created via [`GrpcStartParams::builder`].
pub struct GrpcStartParamsBuilder {
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    login_limiter: Option<Arc<LoginRateLimiter>>,
    ip_login_limiter: Option<Arc<LoginRateLimiter>>,
    mfa_limiter: Option<Arc<LoginRateLimiter>>,
    ip_mfa_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    password_provider: Option<SharedPasswordProvider>,
    rate_limit_backend: Option<SharedRateLimitBackend>,
    infra: Option<Arc<AppInfra>>,
}

impl GrpcStartParamsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: None,
            config_dir: None,
            login_limiter: None,
            ip_login_limiter: None,
            mfa_limiter: None,
            ip_mfa_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
            password_provider: None,
            rate_limit_backend: None,
            infra: None,
        }
    }

    #[must_use]
    pub fn config(mut self, config: CrapConfig) -> Self {
        self.config = Some(config);

        self
    }

    #[must_use]
    pub fn config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);

        self
    }

    #[must_use]
    pub fn login_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.login_limiter = Some(limiter);

        self
    }

    #[must_use]
    pub fn ip_login_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_login_limiter = Some(limiter);

        self
    }

    #[must_use]
    pub fn mfa_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.mfa_limiter = Some(limiter);
        self
    }

    #[must_use]
    pub fn ip_mfa_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_mfa_limiter = Some(limiter);
        self
    }

    #[must_use]
    pub fn forgot_password_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.forgot_password_limiter = Some(limiter);

        self
    }

    #[must_use]
    pub fn ip_forgot_password_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_forgot_password_limiter = Some(limiter);

        self
    }

    #[must_use]
    pub fn password_provider(mut self, password_provider: SharedPasswordProvider) -> Self {
        self.password_provider = Some(password_provider);

        self
    }

    #[must_use]
    pub fn rate_limit_backend(mut self, backend: SharedRateLimitBackend) -> Self {
        self.rate_limit_backend = Some(backend);

        self
    }

    /// Process-stable [`AppInfra`] assembled once at boot.
    #[must_use]
    pub fn infra(mut self, infra: Arc<AppInfra>) -> Self {
        self.infra = Some(infra);

        self
    }

    /// # Panics
    ///
    /// Panics if any required field (`pool`, `registry`, `hook_runner`,
    /// `config`, `config_dir`, `login_limiter`, `ip_login_limiter`, etc.)
    /// was not set on the builder.
    #[must_use]
    pub fn build(self) -> GrpcStartParams {
        GrpcStartParams {
            config: self.config.expect("config is required"),
            config_dir: self.config_dir.expect("config_dir is required"),
            login_limiter: self.login_limiter.expect("login_limiter is required"),
            ip_login_limiter: self.ip_login_limiter.expect("ip_login_limiter is required"),
            mfa_limiter: self.mfa_limiter.expect("mfa_limiter is required"),
            ip_mfa_limiter: self.ip_mfa_limiter.expect("ip_mfa_limiter is required"),
            forgot_password_limiter: self
                .forgot_password_limiter
                .expect("forgot_password_limiter is required"),
            ip_forgot_password_limiter: self
                .ip_forgot_password_limiter
                .expect("ip_forgot_password_limiter is required"),
            password_provider: self
                .password_provider
                .expect("password_provider is required"),
            rate_limit_backend: self
                .rate_limit_backend
                .expect("rate_limit_backend is required"),
            infra: self.infra.expect("infra is required"),
        }
    }
}

/// Start the gRPC server. Reflection is disabled by default and can be
/// enabled via `config.server.grpc_reflection`.
///
/// # Errors
///
/// Returns an error if the address can't be parsed, the listener can't
/// bind, or the server hits an unrecoverable runtime error.
#[cfg(not(tarpaulin_include))]
pub async fn start(addr: &str, params: GrpcStartParams, shutdown: CancellationToken) -> Result<()> {
    let addr = addr.parse()?;

    let cache_max_age = params.config.cache.max_age_secs;
    let grpc_rate_requests = params.config.server.grpc_rate_limit_requests;
    let grpc_rate_window = params.config.server.grpc_rate_limit_window;
    let grpc_reflection = params.config.server.grpc_reflection;
    let grpc_timeout = params.config.server.grpc_timeout;
    // 32-bit overflow path falls back to gRPC's default (4 MiB) rather than
    // usize::MAX — an effectively-unbounded message size would be a DoS vector.
    let grpc_max_msg =
        usize::try_from(params.config.server.grpc_max_message_size).unwrap_or(4 * 1024 * 1024);
    let cors_layer = params.config.cors.build_layer();

    // All process-stable infrastructure comes pre-assembled in `params.infra`;
    // only the per-surface bits are threaded in alongside it.
    let deps_builder = ContentServiceDeps::builder()
        .config(params.config)
        .config_dir(params.config_dir)
        .login_limiter(params.login_limiter)
        .ip_login_limiter(params.ip_login_limiter)
        .mfa_limiter(params.mfa_limiter)
        .ip_mfa_limiter(params.ip_mfa_limiter)
        .forgot_password_limiter(params.forgot_password_limiter)
        .ip_forgot_password_limiter(params.ip_forgot_password_limiter)
        .password_provider(params.password_provider)
        .infra(params.infra);

    let content_service = ContentService::new(deps_builder.build());

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
    let (health_reporter, health_service) = health_reporter();

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
fn spawn_periodic_cache_clear(cache: SharedCache, interval_secs: u64, shutdown: CancellationToken) {
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
                () = shutdown.cancelled() => break,
            }
        }
    });
}
