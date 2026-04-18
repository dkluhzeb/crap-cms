//! `ContentService` struct definition and its impl blocks.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{Arc, atomic::AtomicUsize},
};

use serde_json::Value;
use tokio_stream::Stream;
use tonic::{Request, Response, Status, metadata::MetadataMap};
use tracing::error;

use crate::{
    api::{
        content::{self, content_api_server::ContentApi},
        handlers::ContentServiceDeps,
    },
    config::{EmailConfig, LocaleConfig, PasswordPolicy, ServerConfig},
    core::{
        AuthUser, CollectionDefinition, JwtSecret, Registry,
        auth::{SharedPasswordProvider, SharedTokenProvider, TokenProvider},
        cache::SharedCache,
        collection::GlobalDefinition,
        email::EmailRenderer,
        event::{InProcessInvalidationBus, SharedEventTransport, SharedInvalidationTransport},
        rate_limit::LoginRateLimiter,
        upload::SharedStorage,
    },
    db::{
        AccessResult, BoxedConnection, DbConnection, DbPool,
        query::{self, SharedPopulateSingleflight, Singleflight},
    },
    hooks::HookRunner,
    service::{self, ServiceContext},
};

/// Implements the gRPC ContentAPI service (Find, Create, Update, Delete, Login, etc.).
#[allow(dead_code)]
pub struct ContentService {
    pub(in crate::api::handlers) pool: DbPool,
    pub(in crate::api::handlers) registry: Arc<Registry>,
    pub(in crate::api::handlers) hook_runner: HookRunner,
    pub(in crate::api::handlers) jwt_secret: JwtSecret,
    pub(in crate::api::handlers) default_depth: i32,
    pub(in crate::api::handlers) max_depth: i32,
    pub(in crate::api::handlers) email_config: EmailConfig,
    pub(in crate::api::handlers) email_renderer: Arc<EmailRenderer>,
    pub(in crate::api::handlers) server_config: ServerConfig,
    pub(in crate::api::handlers) event_transport: Option<SharedEventTransport>,
    pub(in crate::api::handlers) locale_config: LocaleConfig,
    pub(in crate::api::handlers) storage: SharedStorage,
    pub(in crate::api::handlers) login_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::handlers) ip_login_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::handlers) reset_token_expiry: u64,
    pub(in crate::api::handlers) password_policy: PasswordPolicy,
    pub(in crate::api::handlers) forgot_password_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::handlers) ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    /// The token provider for JWT creation and validation.
    pub(in crate::api::handlers) token_provider: SharedTokenProvider,
    /// The password provider for hashing and verification.
    pub(in crate::api::handlers) password_provider: SharedPasswordProvider,
    /// Shared cross-request cache for populated relationship documents.
    /// Uses NoneCache when caching is disabled. Cleared on any write operation.
    pub(in crate::api::handlers) cache: SharedCache,
    pub(in crate::api::handlers) pagination_ctx: query::PaginationCtx,
    /// Cached backend identifier (e.g. `"sqlite"`, `"postgres"`), set once at startup.
    pub(in crate::api::handlers) db_kind: String,
    /// Current number of active gRPC Subscribe streams (for connection limiting).
    pub(in crate::api::handlers) subscribe_connections: Arc<AtomicUsize>,
    /// Maximum allowed concurrent Subscribe streams. 0 = unlimited.
    pub(in crate::api::handlers) max_subscribe_connections: usize,
    /// Per-subscriber outbound send timeout for live-update streams.
    pub(in crate::api::handlers) subscriber_send_timeout_ms: u64,
    /// Transport for signalling that a user's live-update streams must be torn
    /// down (e.g. after lock or hard delete). Always present — even when live
    /// updates are disabled, publishing to it is a no-op.
    pub(in crate::api::handlers) invalidation_transport: SharedInvalidationTransport,
    /// Process-wide singleflight for deduplicating concurrent populate
    /// cache-miss DB fetches across requests.
    pub(in crate::api::handlers) populate_singleflight: SharedPopulateSingleflight,
}

/// Pure helper methods — testable without I/O dependencies.
impl ContentService {
    /// Get a clone of the shared cache handle (for periodic clearing).
    pub fn cache_handle(&self) -> SharedCache {
        self.cache.clone()
    }

    pub(in crate::api::handlers) fn get_collection_def(
        &self,
        slug: &str,
    ) -> Result<CollectionDefinition, Status> {
        self.registry
            .get_collection(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Collection '{}' not found", slug)))
    }

    pub(in crate::api::handlers) fn get_global_def(
        &self,
        slug: &str,
    ) -> Result<GlobalDefinition, Status> {
        self.registry
            .get_global(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Global '{}' not found", slug)))
    }

    /// Extract Bearer token string from gRPC metadata (pure, no I/O).
    pub(in crate::api::handlers) fn extract_token(metadata: &MetadataMap) -> Option<String> {
        metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }
}

/// I/O-bound methods: constructor, DB-backed auth resolution, access checks.
/// Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new gRPC content service with all dependencies.
    pub fn new(deps: ContentServiceDeps) -> Self {
        let default_depth = deps.config.depth.default_depth;
        let max_depth = deps.config.depth.max_depth;
        let pagination_ctx = query::PaginationCtx::new(
            deps.config.pagination.default_limit,
            deps.config.pagination.max_limit,
            deps.config.pagination.is_cursor(),
        );
        let reset_token_expiry = deps.config.auth.reset_token_expiry;
        let db_kind = deps.pool.kind().to_string();
        let max_subscribe_connections = deps.config.live.max_subscribe_connections;
        let subscriber_send_timeout_ms = deps.config.live.subscriber_send_timeout_ms;
        let invalidation_transport: SharedInvalidationTransport = deps
            .invalidation_transport
            .unwrap_or_else(|| Arc::new(InProcessInvalidationBus::new()));
        let populate_singleflight: SharedPopulateSingleflight = deps
            .populate_singleflight
            .unwrap_or_else(|| Arc::new(Singleflight::new()));

        Self {
            pool: deps.pool,
            registry: deps.registry,
            hook_runner: deps.hook_runner,
            jwt_secret: deps.jwt_secret,
            default_depth,
            max_depth,
            email_config: deps.config.email,
            email_renderer: deps.email_renderer,
            server_config: deps.config.server,
            event_transport: deps.event_transport,
            locale_config: deps.config.locale,
            storage: deps.storage,
            token_provider: deps.token_provider,
            password_provider: deps.password_provider,
            login_limiter: deps.login_limiter,
            ip_login_limiter: deps.ip_login_limiter,
            reset_token_expiry,
            password_policy: deps.config.auth.password_policy,
            forgot_password_limiter: deps.forgot_password_limiter,
            ip_forgot_password_limiter: deps.ip_forgot_password_limiter,
            cache: deps.cache,
            pagination_ctx,
            db_kind,
            subscribe_connections: Arc::new(AtomicUsize::new(0)),
            max_subscribe_connections,
            subscriber_send_timeout_ms,
            invalidation_transport,
            populate_singleflight,
        }
    }

    /// Resolve an auth user from a token using an existing connection.
    ///
    /// Returns `Ok(None)` when no token is present (anonymous), `Ok(Some(user))`
    /// for a valid token, or `Err(Status::unauthenticated)` for an invalid/expired token.
    ///
    /// Pure data lookup — safe to call inside `spawn_blocking`.
    pub(in crate::api::handlers) fn resolve_auth_user(
        token: Option<String>,
        token_provider: &dyn TokenProvider,
        registry: &Registry,
        conn: &dyn DbConnection,
    ) -> Result<Option<AuthUser>, Status> {
        let token = match token {
            Some(t) => t,
            None => return Ok(None),
        };
        let claims = token_provider
            .validate_token(&token)
            .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;
        let def = match registry.get_collection(&claims.collection) {
            Some(d) => d.clone(),
            None => return Err(Status::unauthenticated("Auth collection no longer exists")),
        };
        // Auth infrastructure — direct query for user lookup, not a user-facing read.
        let doc = match query::find_by_id(conn, &claims.collection, &def, &claims.sub, None) {
            Ok(Some(d)) => d,
            Ok(None) => return Err(Status::unauthenticated("User no longer exists")),
            Err(_) => return Err(Status::unauthenticated("User lookup failed")),
        };

        // Reject tokens with stale session version (password was changed).
        // On DB error, reject the token — do not silently default to 0 which
        // would let stale tokens through during transient failures.
        let ctx = ServiceContext::slug_only(&claims.collection)
            .conn(conn)
            .build();
        let db_session_version = service::auth::get_session_version(&ctx, &claims.sub)
            .map_err(|_| Status::unauthenticated("Session version lookup failed"))?;

        if claims.session_version != db_session_version {
            return Err(Status::unauthenticated("Session invalidated"));
        }

        Ok(Some(AuthUser::new(claims, doc)))
    }

    /// Check collection-level access using an existing connection.
    ///
    /// Free-standing helper — safe to call inside `spawn_blocking`.
    pub(in crate::api::handlers) fn check_access_blocking(
        access_ref: Option<&str>,
        auth_user: &Option<AuthUser>,
        id: Option<&str>,
        data: Option<&HashMap<String, Value>>,
        hook_runner: &HookRunner,
        conn: &mut BoxedConnection,
    ) -> Result<AccessResult, Status> {
        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
        let tx = conn.transaction().map_err(|e| {
            error!("Access check tx error: {}", e);

            Status::internal("Internal error")
        })?;
        let result = hook_runner
            .check_access(access_ref, user_doc, id, data, &tx)
            .map_err(|e| {
                error!("Access check error: {}", e);

                Status::internal("Internal error")
            })?;

        tx.commit().map_err(|e| {
            error!("Access check commit error: {}", e);

            Status::internal("Internal error")
        })?;
        Ok(result)
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
        self.forgot_password_impl(request).await
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
        self.list_collections_impl(request).await
    }

    async fn describe_collection(
        &self,
        request: Request<content::DescribeCollectionRequest>,
    ) -> Result<Response<content::DescribeCollectionResponse>, Status> {
        self.describe_collection_impl(request).await
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
