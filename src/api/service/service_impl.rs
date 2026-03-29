//! `ContentService` struct definition and its impl blocks.

use std::{
    collections::HashMap,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, atomic::AtomicUsize},
};

use serde_json::Value;
use tokio_stream::Stream;
use tonic::{Request, Response, Status, metadata::MetadataMap};

use crate::{
    api::content::{self, content_api_server::ContentApi},
    config::{EmailConfig, LocaleConfig, PasswordPolicy, ServerConfig},
    core::{
        AuthUser, CollectionDefinition, JwtSecret, Registry,
        auth::validate_token,
        collection::GlobalDefinition,
        email::EmailRenderer,
        event::{EventBus, EventUser},
        rate_limit::LoginRateLimiter,
    },
    db::{
        AccessResult, BoxedConnection, DbConnection, DbPool,
        query::{self},
    },
    hooks::HookRunner,
};

use super::ContentServiceDeps;

/// Implements the gRPC ContentAPI service (Find, Create, Update, Delete, Login, etc.).
pub struct ContentService {
    pub(in crate::api::service) pool: DbPool,
    pub(in crate::api::service) registry: Arc<Registry>,
    pub(in crate::api::service) hook_runner: HookRunner,
    pub(in crate::api::service) jwt_secret: JwtSecret,
    pub(in crate::api::service) default_depth: i32,
    pub(in crate::api::service) max_depth: i32,
    pub(in crate::api::service) email_config: EmailConfig,
    pub(in crate::api::service) email_renderer: Arc<EmailRenderer>,
    pub(in crate::api::service) server_config: ServerConfig,
    pub(in crate::api::service) event_bus: Option<EventBus>,
    pub(in crate::api::service) locale_config: LocaleConfig,
    pub(in crate::api::service) config_dir: PathBuf,
    pub(in crate::api::service) login_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::service) ip_login_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::service) reset_token_expiry: u64,
    pub(in crate::api::service) password_policy: PasswordPolicy,
    pub(in crate::api::service) forgot_password_limiter: Arc<LoginRateLimiter>,
    pub(in crate::api::service) ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    /// Shared cross-request cache for populated relationship documents.
    /// None when disabled (default). Cleared on any write operation.
    pub(in crate::api::service) populate_cache: Option<Arc<query::PopulateCache>>,
    pub(in crate::api::service) pagination_ctx: query::PaginationCtx,
    /// Cached backend identifier (e.g. `"sqlite"`, `"postgres"`), set once at startup.
    pub(in crate::api::service) db_kind: String,
    /// Current number of active gRPC Subscribe streams (for connection limiting).
    pub(in crate::api::service) subscribe_connections: Arc<AtomicUsize>,
    /// Maximum allowed concurrent Subscribe streams. 0 = unlimited.
    pub(in crate::api::service) max_subscribe_connections: usize,
}

/// Pure helper methods — testable without I/O dependencies.
impl ContentService {
    /// Get a clone of the shared populate cache handle (for periodic clearing).
    pub fn populate_cache_handle(&self) -> Option<Arc<query::PopulateCache>> {
        self.populate_cache.clone()
    }

    pub(in crate::api::service) fn get_collection_def(
        &self,
        slug: &str,
    ) -> Result<CollectionDefinition, Status> {
        self.registry
            .get_collection(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Collection '{}' not found", slug)))
    }

    pub(in crate::api::service) fn get_global_def(
        &self,
        slug: &str,
    ) -> Result<GlobalDefinition, Status> {
        self.registry
            .get_global(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Global '{}' not found", slug)))
    }

    /// Extract Bearer token string from gRPC metadata (pure, no I/O).
    pub(in crate::api::service) fn extract_token(metadata: &MetadataMap) -> Option<String> {
        metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Extract an EventUser from the gRPC AuthUser (for SSE event attribution).
    pub(in crate::api::service) fn event_user_from(
        auth_user: &Option<AuthUser>,
    ) -> Option<EventUser> {
        auth_user
            .as_ref()
            .map(|au| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()))
    }
}

/// I/O-bound methods: constructor, DB-backed auth resolution, access checks.
/// Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new gRPC content service with all dependencies.
    pub fn new(deps: ContentServiceDeps) -> Self {
        let populate_cache = if deps.config.depth.populate_cache {
            Some(Arc::new(query::PopulateCache::new()))
        } else {
            None
        };
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
            event_bus: deps.event_bus,
            locale_config: deps.config.locale,
            config_dir: deps.config_dir,
            login_limiter: deps.login_limiter,
            ip_login_limiter: deps.ip_login_limiter,
            reset_token_expiry,
            password_policy: deps.config.auth.password_policy,
            forgot_password_limiter: deps.forgot_password_limiter,
            ip_forgot_password_limiter: deps.ip_forgot_password_limiter,
            populate_cache,
            pagination_ctx,
            db_kind,
            subscribe_connections: Arc::new(AtomicUsize::new(0)),
            max_subscribe_connections,
        }
    }

    /// Resolve an auth user from a token using an existing connection.
    ///
    /// Returns `Ok(None)` when no token is present (anonymous), `Ok(Some(user))`
    /// for a valid token, or `Err(Status::unauthenticated)` for an invalid/expired token.
    ///
    /// Pure data lookup — safe to call inside `spawn_blocking`.
    pub(in crate::api::service) fn resolve_auth_user(
        token: Option<String>,
        jwt_secret: &JwtSecret,
        registry: &Registry,
        conn: &dyn DbConnection,
    ) -> Result<Option<AuthUser>, Status> {
        let token = match token {
            Some(t) => t,
            None => return Ok(None),
        };
        let claims = validate_token(&token, jwt_secret.as_ref())
            .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;
        let def = match registry.get_collection(&claims.collection) {
            Some(d) => d.clone(),
            None => return Ok(None),
        };
        let doc = match query::find_by_id(conn, &claims.collection, &def, &claims.sub, None) {
            Ok(Some(d)) => d,
            _ => return Ok(None),
        };

        // Reject tokens with stale session version (password was changed)
        let db_session_version =
            query::get_session_version(conn, &claims.collection, &claims.sub).unwrap_or(0);

        if claims.session_version != db_session_version {
            return Err(Status::unauthenticated("Session invalidated"));
        }

        Ok(Some(AuthUser::new(claims, doc)))
    }

    /// Check collection-level access using an existing connection.
    ///
    /// Free-standing helper — safe to call inside `spawn_blocking`.
    pub(in crate::api::service) fn check_access_blocking(
        access_ref: Option<&str>,
        auth_user: &Option<AuthUser>,
        id: Option<&str>,
        data: Option<&HashMap<String, Value>>,
        hook_runner: &HookRunner,
        conn: &mut BoxedConnection,
    ) -> Result<AccessResult, Status> {
        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
        let tx = conn.transaction().map_err(|e| {
            tracing::error!("Access check tx error: {}", e);
            Status::internal("Internal error")
        })?;
        let result = hook_runner
            .check_access(access_ref, user_doc, id, data, &tx)
            .map_err(|e| {
                tracing::error!("Access check error: {}", e);
                Status::internal("Internal error")
            })?;
        tx.commit().map_err(|e| {
            tracing::error!("Access check commit error: {}", e);
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

    async fn restore(
        &self,
        request: Request<content::RestoreRequest>,
    ) -> Result<Response<content::RestoreResponse>, Status> {
        self.restore_impl(request).await
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::{Document, DocumentId, Slug, auth::ClaimsBuilder};

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

    // ── event_user_from tests ─────────────────────────────────────────

    #[test]
    fn event_user_from_none() {
        assert!(ContentService::event_user_from(&None).is_none());
    }

    #[test]
    fn event_user_from_some() {
        let claims = ClaimsBuilder::new(DocumentId::new("user-123"), Slug::new("users"))
            .email("test@example.com")
            .exp(9999999999)
            .build()
            .unwrap();
        let doc = Document::builder(DocumentId::new("user-123")).build();
        let auth_user = Some(AuthUser::new(claims, doc));
        let event_user = ContentService::event_user_from(&auth_user).unwrap();
        assert_eq!(event_user.id, "user-123");
        assert_eq!(event_user.email, "test@example.com");
    }
}
