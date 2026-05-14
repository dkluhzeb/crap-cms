//! Account management handlers: lock, unlock, verify, unverify.

use std::sync::Arc;

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{content, handlers::ContentService},
    core::{Registry, SharedInvalidationTransport, SharedTokenProvider},
    db::DbPool,
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
            "Collection '{collection}' is not an auth collection"
        )));
    }

    Ok(())
}

/// Owned bundle for an account-action spawn-blocking body.
///
/// `invalidation_transport` is wired only for the `lock_user` flow (which
/// publishes a user-revocation signal so live subscribers tear down the
/// session). Other actions ignore it.
struct AccountActionBlockingInput {
    pool: DbPool,
    token_provider: SharedTokenProvider,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    id: String,
    token: Option<String>,
    invalidation_transport: Option<SharedInvalidationTransport>,
}

/// Resolve auth, then call one of `lock_user`/`unlock_user`/`mark_verified`/
/// `mark_unverified`. The action is taken as a fn pointer so the closure
/// passed to `spawn_blocking` is a single fn call.
fn account_action_blocking(
    input: AccountActionBlockingInput,
    action: fn(&ServiceContext, &str) -> Result<(), ServiceError>,
) -> Result<(), Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token,
        &*input.token_provider,
        &input.registry,
        &conn,
    )?;

    if auth_user.is_none() {
        return Err(Status::unauthenticated("Authentication required"));
    }

    let ctx = ServiceContext::slug_only(&input.collection)
        .conn(&conn)
        .invalidation_transport(input.invalidation_transport)
        .build();

    action(&ctx, &input.id).map_err(|e| Status::from(e.reclassify(&input.db_kind)))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Build the spawn-blocking input bundle from the request, with optional
    /// invalidation transport (used by the lock flow only).
    fn account_action_input(
        &self,
        token: Option<String>,
        req: &content::AccountActionRequest,
        with_invalidation: bool,
    ) -> AccountActionBlockingInput {
        AccountActionBlockingInput {
            pool: self.pool.clone(),
            token_provider: self.token_provider.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            id: req.id.clone(),
            token,
            invalidation_transport: with_invalidation.then(|| self.invalidation_transport.clone()),
        }
    }

    /// Lock a user account, preventing login.
    pub(in crate::api::handlers) async fn lock_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let req = request.into_inner();
        validate_auth_collection(self, &req.collection)?;

        // Service-layer lock_user publishes the invalidation signal
        // when a transport is attached to the context.
        let input = self.account_action_input(token, &req, true);

        task::spawn_blocking(move || account_action_blocking(input, service::auth::lock_user))
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

        let input = self.account_action_input(token, &req, false);

        task::spawn_blocking(move || account_action_blocking(input, service::auth::unlock_user))
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

        let input = self.account_action_input(token, &req, false);

        task::spawn_blocking(move || account_action_blocking(input, service::auth::mark_verified))
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

        let input = self.account_action_input(token, &req, false);

        task::spawn_blocking(move || {
            account_action_blocking(input, service::auth::mark_unverified)
        })
        .await
        .inspect_err(|e| error!("Task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {
            success: true,
        }))
    }
}
