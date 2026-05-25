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
        Registry, SharedCache, SharedEventTransport, SharedInvalidationTransport,
        SharedPasswordProvider, SharedRateLimitBackend, SharedStorage, SharedTokenProvider,
        email::EmailRenderer,
        rate_limit::{GrpcRateLimiter, LoginRateLimiter},
    },
    db::{DbPool, query::SharedPopulateSingleflight},
    hooks::HookRunner,
};

/// Parameters for starting the gRPC API server.
pub struct GrpcStartParams {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub hook_runner: HookRunner,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub event_transport: Option<SharedEventTransport>,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub storage: SharedStorage,
    pub cache: SharedCache,
    pub token_provider: SharedTokenProvider,
    pub password_provider: SharedPasswordProvider,
    pub rate_limit_backend: SharedRateLimitBackend,
    /// Optional shared invalidation transport — when `None`, a fresh in-process
    /// one is created.
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    /// Optional process-wide populate singleflight — when `None`, the
    /// `ContentService` creates a fresh one (dedup only within its own process).
    pub populate_singleflight: Option<SharedPopulateSingleflight>,
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
    pool: Option<DbPool>,
    registry: Option<Arc<Registry>>,
    hook_runner: Option<HookRunner>,
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    event_transport: Option<SharedEventTransport>,
    login_limiter: Option<Arc<LoginRateLimiter>>,
    ip_login_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    storage: Option<SharedStorage>,
    cache: Option<SharedCache>,
    token_provider: Option<SharedTokenProvider>,
    password_provider: Option<SharedPasswordProvider>,
    rate_limit_backend: Option<SharedRateLimitBackend>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
}

impl GrpcStartParamsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            pool: None,
            registry: None,
            hook_runner: None,
            config: None,
            config_dir: None,
            event_transport: None,
            login_limiter: None,
            ip_login_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
            storage: None,
            cache: None,
            token_provider: None,
            password_provider: None,
            rate_limit_backend: None,
            invalidation_transport: None,
            populate_singleflight: None,
        }
    }

    #[must_use]
    pub fn pool(mut self, pool: DbPool) -> Self {
        self.pool = Some(pool);

        self
    }

    #[must_use]
    pub fn registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = Some(registry);

        self
    }

    #[must_use]
    pub fn hook_runner(mut self, hook_runner: HookRunner) -> Self {
        self.hook_runner = Some(hook_runner);

        self
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
    pub fn event_transport(mut self, transport: Option<SharedEventTransport>) -> Self {
        self.event_transport = transport;

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
    pub fn storage(mut self, storage: SharedStorage) -> Self {
        self.storage = Some(storage);

        self
    }

    #[must_use]
    pub fn cache(mut self, cache: SharedCache) -> Self {
        self.cache = Some(cache);

        self
    }

    #[must_use]
    pub fn token_provider(mut self, token_provider: SharedTokenProvider) -> Self {
        self.token_provider = Some(token_provider);

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

    #[must_use]
    pub fn invalidation_transport(mut self, transport: SharedInvalidationTransport) -> Self {
        self.invalidation_transport = Some(transport);

        self
    }

    /// Process-wide populate singleflight shared with the `HookRunner`.
    #[must_use]
    pub fn populate_singleflight(mut self, sf: SharedPopulateSingleflight) -> Self {
        self.populate_singleflight = Some(sf);

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
            pool: self.pool.expect("pool is required"),
            registry: self.registry.expect("registry is required"),
            hook_runner: self.hook_runner.expect("hook_runner is required"),
            config: self.config.expect("config is required"),
            config_dir: self.config_dir.expect("config_dir is required"),
            event_transport: self.event_transport,
            login_limiter: self.login_limiter.expect("login_limiter is required"),
            ip_login_limiter: self.ip_login_limiter.expect("ip_login_limiter is required"),
            forgot_password_limiter: self
                .forgot_password_limiter
                .expect("forgot_password_limiter is required"),
            ip_forgot_password_limiter: self
                .ip_forgot_password_limiter
                .expect("ip_forgot_password_limiter is required"),
            storage: self.storage.expect("storage is required"),
            cache: self.cache.expect("cache is required"),
            token_provider: self.token_provider.expect("token_provider is required"),
            password_provider: self
                .password_provider
                .expect("password_provider is required"),
            rate_limit_backend: self
                .rate_limit_backend
                .expect("rate_limit_backend is required"),
            invalidation_transport: self.invalidation_transport,
            populate_singleflight: self.populate_singleflight,
        }
    }
}

/// Start the gRPC server. Reflection is enabled by default but can be
/// disabled via `config.server.grpc_reflection`.
///
/// # Errors
///
/// Returns an error if the address can't be parsed, the listener can't
/// bind, or the server hits an unrecoverable runtime error.
#[cfg(not(tarpaulin_include))]
pub async fn start(addr: &str, params: GrpcStartParams, shutdown: CancellationToken) -> Result<()> {
    let addr = addr.parse()?;

    let email_renderer = Arc::new(EmailRenderer::new(&params.config_dir)?);

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

    let mut deps_builder = ContentServiceDeps::builder()
        .pool(params.pool)
        .registry(params.registry)
        .hook_runner(params.hook_runner)
        .config(params.config)
        .config_dir(params.config_dir)
        .email_renderer(email_renderer)
        .event_transport(params.event_transport)
        .login_limiter(params.login_limiter)
        .ip_login_limiter(params.ip_login_limiter)
        .forgot_password_limiter(params.forgot_password_limiter)
        .ip_forgot_password_limiter(params.ip_forgot_password_limiter)
        .storage(params.storage)
        .cache(params.cache)
        .token_provider(params.token_provider)
        .password_provider(params.password_provider);

    if let Some(transport) = params.invalidation_transport {
        deps_builder = deps_builder.invalidation_transport(transport);
    }

    if let Some(sf) = params.populate_singleflight {
        deps_builder = deps_builder.populate_singleflight(sf);
    }

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
