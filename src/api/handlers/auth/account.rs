//! Account management handlers: lock, unlock, verify, unverify.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    api::{content, handlers::ContentService},
    core::{Registry, SharedInvalidationTransport, SharedTokenProvider},
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError, auth::AccountAction},
};

/// Shared logic for all account action RPCs.
///
/// Validates auth token, checks collection is auth-enabled, and calls the
/// provided service function inside `spawn_blocking`.
fn validate_auth_collection(registry: &Registry, collection: &str) -> Result<(), Status> {
    let def = registry
        .get_collection(collection)
        .ok_or_else(|| Status::not_found(format!("Collection '{collection}' not found")))?;

    if !def.is_auth_collection() {
        return Err(Status::invalid_argument(format!(
            "Collection '{collection}' is not an auth collection"
        )));
    }

    Ok(())
}

/// `VerifyAccount` / `UnverifyAccount` touch `_verified` and the
/// verification-token columns, which are only provisioned when the
/// auth collection has `verify_email = true`. Calling them on a
/// collection without that feature fails the SQL with "no such
/// column" — without this preflight, that surfaced over the wire as
/// the generic `Status::internal("Internal error")`. `FailedPrecondition`
/// is the correct mapping: the request is well-formed, the server is
/// healthy, but the collection's schema doesn't support the action.
fn validate_verify_email_enabled(registry: &Registry, collection: &str) -> Result<(), Status> {
    let def = registry
        .get_collection(collection)
        .ok_or_else(|| Status::not_found(format!("Collection '{collection}' not found")))?;

    let enabled = def.auth.as_ref().is_some_and(Auth::requires_verify_email);
    if !enabled {
        return Err(Status::failed_precondition(format!(
            "Collection '{collection}' does not have verify_email enabled"
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
    hook_runner: HookRunner,
    registry: Arc<Registry>,
    db_kind: String,
    collection: String,
    id: String,
    token: Option<String>,
    headers: HashMap<String, String>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    /// When true, also require the collection to have `verify_email` enabled
    /// (the `verify`/`unverify` actions). Checked only after authentication so
    /// an unauthenticated caller can't probe collection shape.
    verify_email_required: bool,
}

/// Resolve auth, authorize the caller against the target user, then perform the
/// account action. Authorization is the load-bearing step: a *valid* JWT is not
/// enough — the caller must pass the target collection's `unlock` access
/// (lock/unlock) or `update` access (verify/unverify) against the target user
/// document, so an authenticated-but-unprivileged caller can't lock/verify
/// another account. Enforcement lives in `service::auth::perform_account_action`.
fn account_action_blocking(
    input: AccountActionBlockingInput,
    action: AccountAction,
) -> Result<(), Status> {
    let conn = input
        .pool
        .get()
        .map_err(|e| Status::from(ServiceError::classify(e, &input.db_kind)))?;

    let auth_user = ContentService::resolve_auth_user(
        input.token.as_deref(),
        &input.headers,
        &*input.token_provider,
        &input.hook_runner,
        &input.registry,
        &conn,
    )?;

    let Some(auth_user) = auth_user else {
        return Err(Status::unauthenticated("Authentication required"));
    };

    // Collection-shape validation runs only after authentication so an
    // unauthenticated caller can't probe whether a collection exists, is an
    // auth collection, or has verify_email enabled.
    validate_auth_collection(&input.registry, &input.collection)?;
    if input.verify_email_required {
        validate_verify_email_enabled(&input.registry, &input.collection)?;
    }

    let def = input
        .registry
        .get_collection(&input.collection)
        .ok_or_else(|| Status::not_found(format!("Collection '{}' not found", input.collection)))?;

    let ctx = ServiceContext::collection(&input.collection, def)
        .conn(&conn)
        .runner(&input.hook_runner)
        .user(Some(&auth_user.user_doc))
        .invalidation_transport(input.invalidation_transport)
        .build();

    service::auth::perform_account_action(&ctx, &input.id, action)
        .map_err(|e| Status::from(e.reclassify(&input.db_kind)))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Build the spawn-blocking input bundle from the request. The invalidation
    /// transport (attached for the revoking flows) and the verify-email
    /// requirement are both derived from the `action` itself, so a call site
    /// can't pass a flag that contradicts the action.
    fn account_action_input(
        &self,
        token: Option<String>,
        headers: HashMap<String, String>,
        req: &content::AccountActionRequest,
        action: AccountAction,
    ) -> AccountActionBlockingInput {
        AccountActionBlockingInput {
            pool: self.pool.clone(),
            token_provider: self.token_provider.clone(),
            hook_runner: self.hook_runner.clone(),
            registry: Arc::clone(&self.registry),
            db_kind: self.db_kind.clone(),
            collection: req.collection.clone(),
            id: req.id.clone(),
            token,
            headers,
            invalidation_transport: action
                .invalidates_sessions()
                .then(|| self.invalidation_transport.clone()),
            verify_email_required: action.is_verification_action(),
        }
    }

    /// Lock a user account, preventing login.
    pub(in crate::api::handlers) async fn lock_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        // Service-layer lock_user publishes the invalidation signal
        // when a transport is attached to the context. Collection-shape
        // validation happens after auth inside the blocking body.
        let action = AccountAction::Lock;
        let input = self.account_action_input(token, headers, &req, action);

        task::spawn_blocking(move || account_action_blocking(input, action))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {}))
    }

    /// Unlock a user account, re-enabling login.
    pub(in crate::api::handlers) async fn unlock_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let action = AccountAction::Unlock;
        let input = self.account_action_input(token, headers, &req, action);

        task::spawn_blocking(move || account_action_blocking(input, action))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {}))
    }

    /// Mark a user's email as verified.
    pub(in crate::api::handlers) async fn verify_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        let action = AccountAction::Verify;
        let input = self.account_action_input(token, headers, &req, action);

        task::spawn_blocking(move || account_action_blocking(input, action))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {}))
    }

    /// Mark a user's email as unverified.
    pub(in crate::api::handlers) async fn unverify_account_impl(
        &self,
        request: Request<content::AccountActionRequest>,
    ) -> Result<Response<content::AccountActionResponse>, Status> {
        let metadata = request.metadata().clone();
        let token = Self::extract_token(&metadata);
        let headers = self.metadata_headers(&metadata);
        let req = request.into_inner();

        // Un-verifying revokes login (when verify-email is required), so it must
        // tear down the user's open live streams like lock/password-reset —
        // `mark_unverified` publishes the invalidation only when a transport is
        // attached, which `AccountAction::Unverify::invalidates_sessions()` now
        // guarantees (a missing flag once left streams running on a revoked
        // session).
        let action = AccountAction::Unverify;
        let input = self.account_action_input(token, headers, &req, action);

        task::spawn_blocking(move || account_action_blocking(input, action))
            .await
            .inspect_err(|e| error!("Task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        Ok(Response::new(content::AccountActionResponse {}))
    }
}
