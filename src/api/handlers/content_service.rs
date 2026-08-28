//! `ContentService` struct definition and its impl blocks.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, atomic::AtomicUsize},
};

use tokio::task;
use tokio_stream::Stream;
use tonic::{Request, Response, Status, metadata::MetadataMap};
use tracing::error;

use crate::{
    api::{
        content::{self, content_api_server::ContentApi},
        handlers::ContentServiceDeps,
    },
    config::ServerConfig,
    core::{
        AuthUser, CollectionDefinition, GlobalDefinition, Registry, SharedCache,
        SharedPasswordProvider, SharedTokenProvider, auth::TokenProvider, collection::Surface,
        rate_limit::LoginRateLimiter,
    },
    db::{AccessResult, BoxedConnection, DbConnection, DbPool, query},
    hooks::{AccessCheckInput, HookRunner},
    service::{
        self, AppInfra,
        auth::{AuthFailure, AuthRequest, EvaluateDeps, Resolution},
        op::{CoreError, TargetKind},
    },
};

/// Implements the gRPC `ContentAPI` service (Find, Create, Update, Delete, Login, etc.).
pub struct ContentService {
    pub(in crate::api::handlers) default_depth: i32,
    pub(in crate::api::handlers) max_depth: i32,
    pub(in crate::api::handlers) server_config: ServerConfig,
    /// `[jobs.queues.<name>] retries` snapshot, used by gRPC
    /// `TriggerJob` to compute the effective `max_attempts` for jobs
    /// defined without an explicit `retries` field. Only populated for
    /// queues whose `retries` is `Some`.
    pub(in crate::api::handlers) queue_retries: HashMap<String, u32>,
    /// Cached `registry.has_any_strategy()` result. Lets per-request
    /// helpers like [`metadata_headers`](Self::metadata_headers)
    /// short-circuit work that only the strategy evaluator consumes —
    /// for the common deployment with no strategy methods configured,
    /// we skip materialising the gRPC metadata into a `HashMap` on
    /// every request, eliminating the per-request allocation churn
    /// that dominated the profile.
    pub(in crate::api::handlers) has_strategies: bool,
    /// Whether any `Activation::Always` strategy exists for this
    /// surface. When true, every anonymous request needs the full
    /// metadata `HashMap` materialised because the strategy fires
    /// unconditionally; when false, the `HashMap` is only built when a
    /// header-activated strategy's discriminator is actually present
    /// in the request.
    pub(in crate::api::handlers) has_always_strategy: bool,
    /// Lowercased set of header names that any header-activated
    /// strategy fires on. Used by [`metadata_headers`] to pre-scan
    /// the gRPC `MetadataMap` for a match before allocating the
    /// full `HashMap` — anonymous requests on a deployment whose
    /// only strategy is gated by `x-api-key` pay zero per-request
    /// allocation when no `x-api-key` is on the wire.
    pub(in crate::api::handlers) wanted_strategy_headers: std::collections::HashSet<String>,
    pub(in crate::api::handlers) login_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::handlers) ip_login_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::handlers) reset_token_expiry: u64,
    pub(in crate::api::handlers) forgot_password_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::handlers) ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    /// The password provider for hashing and verification.
    pub(in crate::api::handlers) password_provider: SharedPasswordProvider,
    pub(in crate::api::handlers) pagination_ctx: query::PaginationCtx,
    /// Cached backend identifier (e.g. `"sqlite"`, `"postgres"`), set once at startup.
    pub(in crate::api::handlers) db_kind: String,
    /// Current number of active gRPC Subscribe streams (for connection limiting).
    pub(in crate::api::handlers) subscribe_connections: Arc<AtomicUsize>,
    /// Maximum allowed concurrent Subscribe streams. 0 = unlimited.
    pub(in crate::api::handlers) max_subscribe_connections: usize,
    /// Per-subscriber outbound send timeout for live-update streams.
    pub(in crate::api::handlers) subscriber_send_timeout_ms: u64,
    /// Process-stable infrastructure bundle (pool, registry, hook runner, caches,
    /// transports, providers, config-derived infra), assembled once at boot and
    /// shared across surfaces. Handlers thread it into a `ServiceContext` via
    /// `.infra(&self.infra)`, and read individual process-stable dependencies
    /// (`self.infra.pool`, `self.infra.registry`, …) straight from it.
    pub(in crate::api::handlers) infra: Arc<AppInfra>,
}

/// Pure helper methods — testable without I/O dependencies.
impl ContentService {
    /// Get a clone of the shared cache handle (for periodic clearing).
    #[must_use]
    pub fn cache_handle(&self) -> SharedCache {
        self.infra.cache.clone()
    }

    pub(in crate::api::handlers) fn get_collection_def(
        &self,
        slug: &str,
    ) -> Result<CollectionDefinition, Status> {
        self.infra
            .registry
            .get_collection(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Collection '{slug}' not found")))
    }

    pub(in crate::api::handlers) fn get_global_def(
        &self,
        slug: &str,
    ) -> Result<GlobalDefinition, Status> {
        self.infra
            .registry
            .get_global(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Global '{slug}' not found")))
    }

    /// Extract Bearer token string from gRPC metadata (pure, no I/O).
    pub(in crate::api::handlers) fn extract_token(metadata: &MetadataMap) -> Option<String> {
        metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string)
    }

    /// Snapshot ASCII gRPC metadata into a plain `HashMap` for the
    /// unified auth evaluator and for passing as `ctx.headers` to Lua
    /// strategy hooks. Keys keep their original (gRPC-normalized,
    /// lowercase) casing; case-insensitive matching is the evaluator's
    /// job inside `activation_matches`. Binary metadata is skipped
    /// silently — strategies that need it would need a dedicated
    /// channel.
    pub(in crate::api::handlers) fn extract_metadata_headers(
        metadata: &MetadataMap,
    ) -> HashMap<String, String> {
        let mut out = HashMap::with_capacity(metadata.len());
        for entry in metadata.iter() {
            if let tonic::metadata::KeyAndValueRef::Ascii(name, value) = entry
                && let Ok(v) = value.to_str()
            {
                out.insert(name.as_str().to_string(), v.to_string());
            }
        }
        out
    }

    /// Per-request wrapper around [`extract_metadata_headers`] that
    /// only materialises the `HashMap` when a strategy is actually
    /// going to consume it.
    ///
    /// Decision order:
    /// 1. No strategy methods configured at all → return empty
    ///    immediately (deployments that use only `password_login` /
    ///    `bearer` / `session_cookie` pay nothing).
    /// 2. An `Activation::Always` strategy exists for this surface
    ///    → materialise; the strategy fires unconditionally and
    ///    needs the headers.
    /// 3. Otherwise → pre-scan the `MetadataMap` for any
    ///    header-activated strategy's discriminator. Materialise
    ///    only if one is present; return empty if none match.
    ///
    /// For the typical deployment with a single `x-api-key`-gated
    /// strategy, anonymous traffic that doesn't send `x-api-key`
    /// pays zero per-request allocation here — was the dominant
    /// allocation source on the read hot path before this fix.
    pub(in crate::api::handlers) fn metadata_headers(
        &self,
        metadata: &MetadataMap,
    ) -> HashMap<String, String> {
        if !self.has_strategies {
            return HashMap::new();
        }
        if self.has_always_strategy {
            return Self::extract_metadata_headers(metadata);
        }
        let any_wanted = metadata.iter().any(|entry| {
            matches!(
                entry,
                tonic::metadata::KeyAndValueRef::Ascii(name, _)
                    if self.wanted_strategy_headers.contains(name.as_str())
            )
        });
        if any_wanted {
            Self::extract_metadata_headers(metadata)
        } else {
            HashMap::new()
        }
    }
}

/// Owned inputs for [`ContentService::resolve_schema_auth_blocking`] — grouped
/// so the `spawn_blocking` body is a single named call rather than inline logic.
struct SchemaAuthBlockingInput {
    pool: DbPool,
    token: Option<String>,
    headers: HashMap<String, String>,
    token_provider: SharedTokenProvider,
    hook_runner: HookRunner,
    registry: Arc<Registry>,
}

/// I/O-bound methods: constructor, DB-backed auth resolution, access checks.
/// Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new gRPC content service with all dependencies.
    #[must_use]
    pub fn new(deps: ContentServiceDeps) -> Self {
        let default_depth = deps.config.depth.default_depth;
        let max_depth = deps.config.depth.max_depth;
        let pagination_ctx = query::PaginationCtx::from_config(&deps.config.pagination);
        let reset_token_expiry = deps.config.auth.reset_token_expiry;
        let db_kind = deps.infra.pool.kind().to_string();
        let max_subscribe_connections = deps.config.live.max_subscribe_connections;
        let subscriber_send_timeout_ms = deps.config.live.subscriber_send_timeout_ms;

        let has_strategies = deps.infra.registry.has_any_strategy();
        let has_always_strategy = deps
            .infra
            .registry
            .always_strategies
            .get(&Surface::Grpc)
            .is_some_and(|v| !v.is_empty());
        let wanted_strategy_headers: std::collections::HashSet<String> = deps
            .infra
            .registry
            .header_strategies
            .keys()
            .filter(|(_, surface)| *surface == Surface::Grpc)
            .map(|(header, _)| header.clone())
            .collect();

        let queue_retries = deps
            .config
            .jobs
            .queues
            .iter()
            .filter_map(|(name, q)| q.retries.map(|r| (name.clone(), r)))
            .collect();

        Self {
            default_depth,
            max_depth,
            server_config: deps.config.server,
            queue_retries,
            has_strategies,
            has_always_strategy,
            wanted_strategy_headers,
            login_limiter: deps.login_limiter,
            ip_login_limiter: deps.ip_login_limiter,
            reset_token_expiry,
            forgot_password_limiter: deps.forgot_password_limiter,
            ip_forgot_password_limiter: deps.ip_forgot_password_limiter,
            password_provider: deps.password_provider,
            pagination_ctx,
            db_kind,
            subscribe_connections: Arc::new(AtomicUsize::new(0)),
            max_subscribe_connections,
            subscriber_send_timeout_ms,
            infra: deps.infra,
        }
    }

    /// Resolve a request's principal via the unified auth evaluator.
    ///
    /// Returns `Ok(None)` when no method matched (anonymous request),
    /// `Ok(Some(user))` when a method authenticated the request, or
    /// `Err(Status::unauthenticated)` when a credential was supplied
    /// but invalid (bad signature, stale session, revoked user).
    ///
    /// Honors per-method `surfaces` and `activates_on`: a strategy
    /// only fires when its activation discriminator matches the
    /// current request, and a Bearer JWT is only accepted on
    /// surfaces the issuing collection explicitly listed.
    ///
    /// Pure data lookup (Lua may fire if a strategy matches) — safe
    /// to call inside `spawn_blocking`.
    pub(in crate::api::handlers) fn resolve_auth_user(
        bearer: Option<&str>,
        headers: &HashMap<String, String>,
        token_provider: &dyn TokenProvider,
        hook_runner: &HookRunner,
        registry: &Registry,
        conn: &dyn DbConnection,
    ) -> Result<Option<AuthUser>, Status> {
        let request = AuthRequest {
            surface: Surface::Grpc,
            bearer_token: bearer,
            session_cookie_token: None,
            headers,
        };
        let deps = EvaluateDeps {
            registry,
            token_provider,
            hook_runner,
            conn,
        };
        match service::auth::evaluate(&request, &deps) {
            Resolution::Authenticated(auth) => Ok(Some(auth.user)),
            Resolution::Anonymous => Ok(None),
            Resolution::Invalid(failure) => Err(auth_failure_status(failure)),
        }
    }

    /// Pull a connection and resolve the authenticated user. The
    /// `spawn_blocking` body for schema-introspection auth — extracted so the
    /// closure is a single function call, never inline business logic.
    fn resolve_schema_auth_blocking(
        input: &SchemaAuthBlockingInput,
    ) -> Result<Option<AuthUser>, Status> {
        let conn = input
            .pool
            .get()
            .inspect_err(|e| error!("Schema introspection auth pool error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;

        Self::resolve_auth_user(
            input.token.as_deref(),
            &input.headers,
            &*input.token_provider,
            &input.hook_runner,
            &input.registry,
            &conn,
        )
    }

    /// Check collection-level access using an existing connection.
    ///
    /// Free-standing helper — safe to call inside `spawn_blocking`.
    pub(in crate::api::handlers) fn check_access_blocking(
        input: &AccessCheckInput<'_>,
        hook_runner: &HookRunner,
        conn: &mut BoxedConnection,
    ) -> Result<AccessResult, Status> {
        let tx = conn
            .transaction()
            .inspect_err(|e| error!("Access check tx error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;
        let result = hook_runner
            .check_access(input, &tx)
            .inspect_err(|e| error!("Access check error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;

        tx.commit()
            .inspect_err(|e| error!("Access check commit error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;
        Ok(result)
    }

    /// Gate the schema-introspection RPCs on authentication when
    /// `[server] public_schema_introspection = false`. Public (the default) is a
    /// no-op; otherwise an anonymous caller is rejected before the schema shape
    /// is returned. Document data is unaffected — always access-gated.
    async fn require_schema_introspection_auth(
        &self,
        metadata: &MetadataMap,
    ) -> Result<(), Status> {
        if self.server_config.public_schema_introspection {
            return Ok(());
        }

        let blocking_input = SchemaAuthBlockingInput {
            token: Self::extract_token(metadata),
            headers: self.metadata_headers(metadata),
            pool: self.infra.pool.clone(),
            token_provider: self.infra.token_provider.clone(),
            hook_runner: self.infra.hook_runner.clone(),
            registry: Arc::clone(&self.infra.registry),
        };

        let authed =
            task::spawn_blocking(move || Self::resolve_schema_auth_blocking(&blocking_input))
                .await
                .inspect_err(|e| error!("Schema introspection auth task error: {}", e))
                .map_err(|_| Status::internal("Internal error"))??;

        if authed.is_none() {
            return Err(Status::unauthenticated(
                "Authentication required for schema introspection",
            ));
        }

        Ok(())
    }

    /// Map a core-dispatch error onto the gRPC wire. The single translation
    /// point for every handler ported to `service::op::run_blocking` — auth
    /// failures keep their precise statuses, service errors reuse the
    /// existing `Status` conversion, and infra failures go through
    /// `ServiceError::classify` for backend-aware busy/timeout mapping.
    pub(in crate::api::handlers) fn core_error_status(&self, e: CoreError) -> Status {
        match e {
            CoreError::Auth(failure) => auth_failure_status(failure),
            CoreError::UnknownTarget { slug, kind } => Status::not_found(match kind {
                TargetKind::Collection => format!("Collection '{slug}' not found"),
                TargetKind::Global => format!("Global '{slug}' not found"),
            }),
            CoreError::Service(e) => Status::from(e),
            CoreError::Internal(e) => {
                Status::from(service::ServiceError::classify(e, &self.db_kind))
            }
        }
    }
}

/// Precise per-failure gRPC statuses for credential rejection. None expose
/// user-existence — `Locked` and `StaleSession` only surface when the bearer
/// already proved knowledge of a valid signed token for that user.
pub(in crate::api::handlers) fn auth_failure_status(failure: AuthFailure) -> Status {
    match failure {
        AuthFailure::Locked => Status::permission_denied("Account locked"),
        AuthFailure::StaleSession => Status::unauthenticated("Session invalidated"),
        AuthFailure::UserMissing => Status::unauthenticated("User no longer exists"),
        AuthFailure::UnknownCollection => {
            Status::unauthenticated("Auth collection no longer exists")
        }
        AuthFailure::Lookup => Status::unavailable("User lookup failed"),
        AuthFailure::BadToken => Status::unauthenticated("Invalid or expired token"),
        AuthFailure::Unaccepted => {
            Status::unauthenticated("Credential not accepted on this surface")
        }
    }
}

/// Untestable as unit: all methods are async gRPC handlers requiring full server + Lua VM + DB.
/// Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
#[tonic::async_trait]
impl ContentApi for ContentService {
    async fn find(
        &self,
        request: Request<content::FindRequest>,
    ) -> Result<Response<content::FindResponse>, Status> {
        self.find_impl(request).await
    }

    async fn find_by_id(
        &self,
        request: Request<content::FindByIdRequest>,
    ) -> Result<Response<content::FindByIdResponse>, Status> {
        self.find_by_id_impl(request).await
    }

    async fn create(
        &self,
        request: Request<content::CreateRequest>,
    ) -> Result<Response<content::CreateResponse>, Status> {
        self.create_impl(request).await
    }

    async fn update(
        &self,
        request: Request<content::UpdateRequest>,
    ) -> Result<Response<content::UpdateResponse>, Status> {
        self.update_impl(request).await
    }

    async fn delete(
        &self,
        request: Request<content::DeleteRequest>,
    ) -> Result<Response<content::DeleteResponse>, Status> {
        self.delete_impl(request).await
    }

    async fn undelete(
        &self,
        request: Request<content::UndeleteRequest>,
    ) -> Result<Response<content::UndeleteResponse>, Status> {
        self.undelete_impl(request).await
    }

    async fn count(
        &self,
        request: Request<content::CountRequest>,
    ) -> Result<Response<content::CountResponse>, Status> {
        self.count_impl(request).await
    }

    async fn create_many(
        &self,
        request: Request<content::CreateManyRequest>,
    ) -> Result<Response<content::CreateManyResponse>, Status> {
        self.create_many_impl(request).await
    }

    async fn update_many(
        &self,
        request: Request<content::UpdateManyRequest>,
    ) -> Result<Response<content::UpdateManyResponse>, Status> {
        self.update_many_impl(request).await
    }

    async fn delete_many(
        &self,
        request: Request<content::DeleteManyRequest>,
    ) -> Result<Response<content::DeleteManyResponse>, Status> {
        self.delete_many_impl(request).await
    }

    async fn get_global(
        &self,
        request: Request<content::GetGlobalRequest>,
    ) -> Result<Response<content::GetGlobalResponse>, Status> {
        self.get_global_impl(request).await
    }

    async fn update_global(
        &self,
        request: Request<content::UpdateGlobalRequest>,
    ) -> Result<Response<content::UpdateGlobalResponse>, Status> {
        self.update_global_impl(request).await
    }

    async fn login(
        &self,
        request: Request<content::LoginRequest>,
    ) -> Result<Response<content::LoginResponse>, Status> {
        self.login_impl(request).await
    }

    async fn forgot_password(
        &self,
        request: Request<content::ForgotPasswordRequest>,
    ) -> Result<Response<content::ForgotPasswordResponse>, Status> {
        Ok(self.forgot_password_impl(request))
    }

    async fn reset_password(
        &self,
        request: Request<content::ResetPasswordRequest>,
    ) -> Result<Response<content::ResetPasswordResponse>, Status> {
        self.reset_password_impl(request).await
    }

    async fn verify_email(
        &self,
        request: Request<content::VerifyEmailRequest>,
    ) -> Result<Response<content::VerifyEmailResponse>, Status> {
        self.verify_email_impl(request).await
    }

    async fn list_collections(
        &self,
        request: Request<content::ListCollectionsRequest>,
    ) -> Result<Response<content::ListCollectionsResponse>, Status> {
        self.require_schema_introspection_auth(request.metadata())
            .await?;
        Ok(self.list_collections_impl(request))
    }

    async fn describe_collection(
        &self,
        request: Request<content::DescribeCollectionRequest>,
    ) -> Result<Response<content::DescribeCollectionResponse>, Status> {
        self.require_schema_introspection_auth(request.metadata())
            .await?;
        self.describe_collection_impl(request)
    }

    type SubscribeStream =
        Pin<Box<dyn Stream<Item = Result<content::MutationEvent, Status>> + Send>>;

    async fn subscribe(
        &self,
        request: Request<content::SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        self.subscribe_impl(request).await
    }

    async fn me(
        &self,
        request: Request<content::MeRequest>,
    ) -> Result<Response<content::MeResponse>, Status> {
        self.me_impl(request).await
    }

    async fn list_versions(
        &self,
        request: Request<content::ListVersionsRequest>,
    ) -> Result<Response<content::ListVersionsResponse>, Status> {
        self.list_versions_impl(request).await
    }

    async fn restore_version(
        &self,
        request: Request<content::RestoreVersionRequest>,
    ) -> Result<Response<content::RestoreVersionResponse>, Status> {
        self.restore_version_impl(request).await
    }

    async fn list_jobs(
        &self,
        request: Request<content::ListJobsRequest>,
    ) -> Result<Response<content::ListJobsResponse>, Status> {
        self.list_jobs_impl(request).await
    }

    async fn trigger_job(
        &self,
        request: Request<content::TriggerJobRequest>,
    ) -> Result<Response<content::TriggerJobResponse>, Status> {
        self.trigger_job_impl(request).await
    }

    async fn get_job_run(
        &self,
        request: Request<content::GetJobRunRequest>,
    ) -> Result<Response<content::GetJobRunResponse>, Status> {
        self.get_job_run_impl(request).await
    }

    async fn list_job_runs(
        &self,
        request: Request<content::ListJobRunsRequest>,
    ) -> Result<Response<content::ListJobRunsResponse>, Status> {
        self.list_job_runs_impl(request).await
    }

    async fn validate(
        &self,
        request: Request<content::ValidateRequest>,
    ) -> Result<Response<content::ValidateResponse>, Status> {
        self.validate_impl(request).await
    }

    async fn validate_global(
        &self,
        request: Request<content::ValidateGlobalRequest>,
    ) -> Result<Response<content::ValidateResponse>, Status> {
        self.validate_global_impl(request).await
    }

    async fn lock_account(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        self.lock_account_impl(request).await
    }

    async fn unlock_account(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        self.unlock_account_impl(request).await
    }

    async fn verify_account(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        self.verify_account_impl(request).await
    }

    async fn unverify_account(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        self.unverify_account_impl(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── extract_token tests ───────────────────────────────────────────

    #[test]
    fn extract_token_valid_bearer() {
        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer abc123".parse().unwrap());
        assert_eq!(
            ContentService::extract_token(&meta),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn extract_token_missing_header() {
        let meta = MetadataMap::new();
        assert_eq!(ContentService::extract_token(&meta), None);
    }

    #[test]
    fn extract_token_wrong_prefix() {
        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Basic abc123".parse().unwrap());
        assert_eq!(ContentService::extract_token(&meta), None);
    }

    #[test]
    fn extract_token_empty_value() {
        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer ".parse().unwrap());
        assert_eq!(ContentService::extract_token(&meta), None);
    }

    #[test]
    fn extract_token_bearer_case_sensitive() {
        let mut meta = MetadataMap::new();
        meta.insert("authorization", "bearer abc123".parse().unwrap());
        // "bearer" (lowercase) should not match "Bearer " prefix
        assert_eq!(ContentService::extract_token(&meta), None);
    }
}
