//! End-to-end gRPC test harness.
//!
//! Mirrors the production `crap_cms::api::server::start` wiring but
//! binds to `127.0.0.1:0` via a caller-owned `TcpListener` so each
//! test gets an ephemeral port we can hand back to the client. The
//! existing main-crate `tests/grpc_*.rs` files exercise the
//! `ContentService` impl directly (in-process trait calls) — those
//! cover business-logic correctness. This harness adds the missing
//! transport-level coverage: real TCP, real `tonic` channel, real
//! `Server::builder` layer stack including health and reflection.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use tokio::{net::TcpListener, task::JoinHandle};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, Server};
use tonic_health::server::health_reporter;

use crap_cms::{
    api::{
        content::{FILE_DESCRIPTOR_SET, content_api_server::ContentApiServer},
        handlers::{ContentService, ContentServiceDeps},
        rate_limit::GrpcRateLimitLayer,
    },
    config::{CrapConfig, UploadConfig},
    core::{
        JwtSecret, Registry,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        cache::NoneCache,
        collection::{CollectionDefinition, GlobalDefinition},
        email::EmailRenderer,
        event::{InProcessEventBus, InProcessInvalidationBus},
        job::JobDefinition,
        rate_limit::{GrpcRateLimiter, LoginRateLimiter, MemoryRateLimitBackend},
        upload::create_storage,
    },
    db::{DbPool, migrate, pool, query::Singleflight},
    hooks::HookRunner,
};

const JWT_SECRET: &str = "test-jwt-secret";

/// Bundle returned by [`spawn_grpc_server`]. The pool + registry +
/// tmpdir let tests seed the DB and read it back; the channel +
/// addr are how tests issue RPCs; the shutdown token + handle let
/// tests stop the server cleanly when needed.
pub struct GrpcTestCtx {
    pub _tmp: tempfile::TempDir,
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub jwt_secret: JwtSecret,
    pub addr: SocketAddr,
    pub channel: Channel,
    pub shutdown: CancellationToken,
    pub server_handle: JoinHandle<()>,
}

impl GrpcTestCtx {
    /// `http://127.0.0.1:PORT` — useful when a test needs to build
    /// a second channel (e.g. an unauthenticated one alongside the
    /// authenticated default).
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

/// Spawn a real `tonic` server on `127.0.0.1:0` with the standard
/// crap-cms layer stack (health + reflection + content service) and
/// return a connected `Channel` plus everything a test needs to
/// seed the DB or stop the server.
///
/// Rate limiting is **off** in this variant — tests usually don't
/// want a rate limiter sitting in front of the requests they're
/// trying to assert on. For tests that explicitly want the layer
/// (e.g. proving `RESOURCE_EXHAUSTED` after a burst), use
/// [`spawn_grpc_server_with_rate_limit`].
///
/// The server runs in a background task. Tests can let the runtime
/// drop everything at the end (cheap), or call
/// `ctx.shutdown.cancel()` and `await ctx.server_handle` for clean
/// teardown when ordering matters (e.g. asserting subscriber
/// drop-on-shutdown semantics).
pub async fn spawn_grpc_server(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
) -> GrpcTestCtx {
    spawn_grpc_server_inner(collections, globals, Vec::new(), None).await
}

/// Like [`spawn_grpc_server`] but also registers `jobs` in the
/// registry so `TriggerJob` / `ListJobs` / `GetJobRun` / `ListJobRuns`
/// have something to enumerate and target.
pub async fn spawn_grpc_server_with_jobs(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    jobs: Vec<JobDefinition>,
) -> GrpcTestCtx {
    spawn_grpc_server_inner(collections, globals, jobs, None).await
}

/// Like [`spawn_grpc_server`] but installs the `GrpcRateLimitLayer`
/// configured for `max_requests` per `window_secs`. The same limiter
/// is shared across all RPCs; counted per-IP (sliding window). Setting
/// `max_requests = 0` disables limiting — use [`spawn_grpc_server`]
/// instead in that case.
pub async fn spawn_grpc_server_with_rate_limit(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    max_requests: u32,
    window_secs: u64,
) -> GrpcTestCtx {
    spawn_grpc_server_inner(
        collections,
        globals,
        Vec::new(),
        Some(GrpcRateLimitLayer::new(Arc::new(
            GrpcRateLimiter::with_backend(
                Arc::new(MemoryRateLimitBackend::new()),
                max_requests,
                window_secs,
            ),
        ))),
    )
    .await
}

async fn spawn_grpc_server_inner(
    collections: Vec<CollectionDefinition>,
    globals: Vec<GlobalDefinition>,
    jobs: Vec<JobDefinition>,
    rate_limit_layer: Option<GrpcRateLimitLayer>,
) -> GrpcTestCtx {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_dir = tmp.path().to_path_buf();
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();
    config.auth.secret = JWT_SECRET.into();

    let pool = pool::create_pool(&config_dir, &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in &collections {
            reg.register_collection(def.clone());
        }
        for def in &globals {
            reg.register_global(def.clone());
        }
        for def in &jobs {
            reg.register_job(def.clone());
        }
    }
    let registry = Registry::snapshot(&shared);

    migrate::sync_all(&pool, &registry, &config.locale).expect("sync schema");

    let hook_runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("create hook runner");

    let email_renderer = Arc::new(EmailRenderer::new(&config_dir).expect("create email renderer"));

    let storage = create_storage(&config_dir, &UploadConfig::default()).expect("create storage");

    let token_provider = Arc::new(JwtTokenProvider::new(JWT_SECRET));
    let password_provider = Arc::new(Argon2PasswordProvider);
    let invalidation_transport = Arc::new(InProcessInvalidationBus::new());
    let populate_singleflight = Arc::new(Singleflight::new());
    let event_transport = Arc::new(InProcessEventBus::new(1024));

    let deps = ContentServiceDeps::builder()
        .pool(pool.clone())
        .registry(Arc::clone(&registry))
        .hook_runner(hook_runner)
        .config(config.clone())
        .config_dir(config_dir.clone())
        .storage(storage)
        .email_renderer(email_renderer)
        .login_limiter(Arc::new(LoginRateLimiter::new(5, 300)))
        .ip_login_limiter(Arc::new(LoginRateLimiter::new(20, 300)))
        .forgot_password_limiter(Arc::new(LoginRateLimiter::new(3, 900)))
        .ip_forgot_password_limiter(Arc::new(LoginRateLimiter::new(20, 900)))
        .cache(Arc::new(NoneCache))
        .token_provider(token_provider)
        .password_provider(password_provider)
        .invalidation_transport(invalidation_transport)
        .populate_singleflight(populate_singleflight)
        .event_transport(Some(event_transport))
        .build();

    let content_svc = ContentApiServer::new(ContentService::new(deps));

    let (health_reporter, health_service) = health_reporter();
    health_reporter
        .set_serving::<ContentApiServer<ContentService>>()
        .await;

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .build_v1()
        .expect("build reflection service");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    let incoming = TcpListenerStream::new(listener);

    let shutdown = CancellationToken::new();
    let shutdown_signal = shutdown.clone().cancelled_owned();

    let server_handle = tokio::spawn(async move {
        Server::builder()
            .layer(tower::util::option_layer(rate_limit_layer))
            .add_service(health_service)
            .add_service(reflection_service)
            .add_service(content_svc)
            .serve_with_incoming_shutdown(incoming, shutdown_signal)
            .await
            .expect("grpc server");
    });

    let channel = Channel::from_shared(format!("http://{addr}"))
        .expect("valid uri")
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("connect to grpc server");

    GrpcTestCtx {
        _tmp: tmp,
        pool,
        registry,
        config,
        config_dir,
        jwt_secret: JWT_SECRET.into(),
        addr,
        channel,
        shutdown,
        server_handle,
    }
}
