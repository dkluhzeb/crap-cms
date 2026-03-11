//! Tonic gRPC service implementing all ContentAPI RPCs.

mod auth;
mod collection;
mod convert;
mod schema_ops;

use std::collections::HashMap;
use std::pin::Pin;
use tokio_stream::Stream;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::api::content::content_api_server::ContentApi;
use crate::config::{EmailConfig, LocaleConfig, ServerConfig};
use crate::core::Registry;
use crate::core::auth::AuthUser;
use crate::core::email::EmailRenderer;
use crate::core::event::EventBus;
use crate::core::event::EventUser;
use crate::core::rate_limit::LoginRateLimiter;
use crate::db::DbPool;
use crate::db::query;
use crate::db::query::AccessResult;
use crate::hooks::lifecycle::HookRunner;

/// Implements the gRPC ContentAPI service (Find, Create, Update, Delete, Login, etc.).
pub struct ContentService {
    pool: DbPool,
    registry: std::sync::Arc<Registry>,
    hook_runner: HookRunner,
    jwt_secret: String,
    default_depth: i32,
    max_depth: i32,
    email_config: EmailConfig,
    email_renderer: std::sync::Arc<EmailRenderer>,
    server_config: ServerConfig,
    event_bus: Option<EventBus>,
    locale_config: LocaleConfig,
    config_dir: std::path::PathBuf,
    login_limiter: std::sync::Arc<LoginRateLimiter>,
    reset_token_expiry: u64,
    password_policy: crate::config::PasswordPolicy,
    forgot_password_limiter: std::sync::Arc<crate::core::rate_limit::LoginRateLimiter>,
    /// Shared cross-request cache for populated relationship documents.
    /// None when disabled (default). Cleared on any write operation.
    populate_cache: Option<std::sync::Arc<query::PopulateCache>>,
    default_limit: i64,
    max_limit: i64,
    cursor_enabled: bool,
}

/// Untestable as unit: helper methods require full pool + registry + hook_runner.
/// Covered by integration tests in tests/ directory.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Create a new gRPC content service with all dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: DbPool,
        registry: std::sync::Arc<Registry>,
        hook_runner: HookRunner,
        jwt_secret: String,
        depth_config: &crate::config::DepthConfig,
        pagination_config: &crate::config::PaginationConfig,
        email_config: EmailConfig,
        email_renderer: std::sync::Arc<EmailRenderer>,
        server_config: ServerConfig,
        event_bus: Option<EventBus>,
        locale_config: LocaleConfig,
        config_dir: std::path::PathBuf,
        login_limiter: std::sync::Arc<LoginRateLimiter>,
        reset_token_expiry: u64,
        password_policy: crate::config::PasswordPolicy,
        forgot_password_limiter: std::sync::Arc<crate::core::rate_limit::LoginRateLimiter>,
    ) -> Self {
        Self {
            pool,
            registry,
            hook_runner,
            jwt_secret,
            default_depth: depth_config.default_depth,
            max_depth: depth_config.max_depth,
            email_config,
            email_renderer,
            server_config,
            event_bus,
            locale_config,
            config_dir,
            login_limiter,
            reset_token_expiry,
            password_policy,
            forgot_password_limiter,
            populate_cache: if depth_config.populate_cache {
                Some(std::sync::Arc::new(query::PopulateCache::new()))
            } else {
                None
            },
            default_limit: pagination_config.default_limit,
            max_limit: pagination_config.max_limit,
            cursor_enabled: pagination_config.is_cursor(),
        }
    }

    /// Get a clone of the shared populate cache handle (for periodic clearing).
    pub fn populate_cache_handle(&self) -> Option<std::sync::Arc<query::PopulateCache>> {
        self.populate_cache.clone()
    }

    #[allow(clippy::result_large_err)]
    fn get_collection_def(&self, slug: &str) -> Result<crate::core::CollectionDefinition, Status> {
        self.registry
            .get_collection(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Collection '{}' not found", slug)))
    }

    #[allow(clippy::result_large_err)]
    fn get_global_def(
        &self,
        slug: &str,
    ) -> Result<crate::core::collection::GlobalDefinition, Status> {
        self.registry
            .get_global(slug)
            .cloned()
            .ok_or_else(|| Status::not_found(format!("Global '{}' not found", slug)))
    }

    /// Extract auth user from gRPC metadata (Bearer token in `authorization` header).
    fn extract_auth_user(&self, metadata: &MetadataMap) -> Option<AuthUser> {
        let token = metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))?;
        let claims = crate::core::auth::validate_token(token, &self.jwt_secret).ok()?;
        let def = self.registry.get_collection(&claims.collection)?.clone();
        let conn = self.pool.get().ok()?;
        let doc = query::find_by_id(&conn, &claims.collection, &def, &claims.sub, None).ok()??;
        Some(AuthUser::new(claims, doc))
    }

    /// Check collection-level access, returning the AccessResult or a Status error.
    #[allow(clippy::result_large_err)]
    fn require_access(
        &self,
        access_ref: Option<&str>,
        auth_user: &Option<AuthUser>,
        id: Option<&str>,
        data: Option<&HashMap<String, serde_json::Value>>,
    ) -> Result<AccessResult, Status> {
        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
        let mut conn = self
            .pool
            .get()
            .map_err(|_| Status::unavailable("Database connection pool exhausted (retryable)"))?;
        let tx = conn.transaction().map_err(|e| {
            tracing::error!("Access check tx error: {}", e);
            Status::internal("Internal error")
        })?;
        let result = self
            .hook_runner
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

    /// Extract an EventUser from the gRPC AuthUser (for SSE event attribution).
    fn event_user_from(auth_user: &Option<AuthUser>) -> Option<EventUser> {
        auth_user
            .as_ref()
            .map(|au| EventUser::new(au.claims.sub.clone(), au.claims.email.clone()))
    }

    /// Strip field-level read-denied fields from a proto document.
    /// Fail closed: on pool/tx error, strip ALL fields that have access controls.
    fn strip_denied_read_fields(
        &self,
        doc: &mut content::Document,
        fields: &[crate::core::field::FieldDefinition],
        auth_user: &Option<AuthUser>,
    ) {
        let user_doc = auth_user.as_ref().map(|au| &au.user_doc);
        let denied = match self.pool.get() {
            Ok(mut conn) => match conn.transaction() {
                Ok(tx) => {
                    let d = self
                        .hook_runner
                        .check_field_read_access(fields, user_doc, &tx);
                    // Read-only access check — commit result is irrelevant, rollback on drop is safe
                    let _ = tx.commit();
                    d
                }
                Err(e) => {
                    tracing::error!("Field access check tx error (fail closed): {}", e);
                    fields
                        .iter()
                        .filter(|f| f.access.read.is_some())
                        .map(|f| f.name.clone())
                        .collect()
                }
            },
            Err(e) => {
                tracing::error!("Field access check pool error (fail closed): {}", e);
                fields
                    .iter()
                    .filter(|f| f.access.read.is_some())
                    .map(|f| f.name.clone())
                    .collect()
            }
        };
        if let Some(ref mut s) = doc.fields {
            for name in &denied {
                s.fields.remove(name);
            }
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
