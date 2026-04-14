//! Account management handlers: lock, unlock, verify, unverify.

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    service::{self, ServiceContext, ServiceError},
};

/// Shared logic for all account action RPCs.
///
/// Validates auth token, checks collection is auth-enabled, and calls the
/// provided service function inside `spawn_blocking`.
fn validate_auth_collection(service: &ContentService, collection: &str) -> Result<(), Status> {
    let def = service.get_collection_def(collection)?;

    if !def.is_auth_collection() {
        return Err(Status::invalid_argument(format!(
            "Collection '{}' is not an auth collection",
            collection
        )));
    }

    Ok(())
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Lock a user account, preventing login.
    pub(in crate::api::handlers) async fn lock_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        validate_auth_collection(self, &req.collection)?;

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();
        let invalidation_transport = self.invalidation_transport.clone();

        task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            // Service-layer lock_user publishes the invalidation signal
            // when a transport is attached to the context.
            let ctx = ServiceContext::slug_only(&collection)
                .conn(&conn)
                .invalidation_transport(Some(invalidation_transport))
                .build();
            service::auth::lock_user(&ctx, &id)
                .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            Ok(())
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {
            success: true,
        }))
    }

    /// Unlock a user account, re-enabling login.
    pub(in crate::api::handlers) async fn unlock_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        validate_auth_collection(self, &req.collection)?;

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();

        task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            let ctx = ServiceContext::slug_only(&collection).conn(&conn).build();
            service::auth::unlock_user(&ctx, &id)
                .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            Ok(())
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {
            success: true,
        }))
    }

    /// Mark a user's email as verified.
    pub(in crate::api::handlers) async fn verify_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        validate_auth_collection(self, &req.collection)?;

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();

        task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            let ctx = ServiceContext::slug_only(&collection).conn(&conn).build();
            service::auth::mark_verified(&ctx, &id)
                .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            Ok(())
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {
            success: true,
        }))
    }

    /// Mark a user's email as unverified.
    pub(in crate::api::handlers) async fn unverify_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        validate_auth_collection(self, &req.collection)?;

        let pool = self.pool.clone();
        let token_provider = self.token_provider.clone();
        let registry = self.registry.clone();
        let db_kind = self.db_kind.clone();
        let collection = req.collection.clone();
        let id = req.id.clone();

        task::spawn_blocking(move || -> Result<_, Status> {
            let conn = pool
                .get()
                .map_err(|e| Status::from(ServiceError::classify(e, &db_kind)))?;

            let auth_user =
                ContentService::resolve_auth_user(token, &*token_provider, &registry, &conn)?;

            if auth_user.is_none() {
                return Err(Status::unauthenticated("Authentication required"));
            }

            let ctx = ServiceContext::slug_only(&collection).conn(&conn).build();
            service::auth::mark_unverified(&ctx, &id)
                .map_err(|e| Status::from(e.reclassify(&db_kind)))?;

            Ok(())
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {
            success: true,
        }))
    }
}
