//! Login handler — authenticate with email/password and return a JWT.

use std::collections::HashMap;

use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    core::{CollectionDefinition, Document, SharedPasswordProvider, Slug, auth::ClaimsBuilder},
    db::DbPool,
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError, auth::authenticate_local},
};

/// Owned bundle for the `Login` spawn-blocking body.
struct LoginBlockingInput {
    pool: DbPool,
    slug: String,
    email: String,
    password: String,
    def: CollectionDefinition,
    check_verify_email: bool,
    disable_local: bool,
    password_provider: SharedPasswordProvider,
    hook_runner: HookRunner,
}

/// Try local email+password auth first, then any configured custom strategies.
/// Returns `Ok(Some((user, session_version)))` on success, `Ok(None)` for any
/// recoverable failure (wrong password, locked, unverified). Errors propagate
/// only for system failures.
fn login_blocking(input: LoginBlockingInput) -> Result<Option<(Document, u64)>, Status> {
    let conn = input
        .pool
        .get()
        .inspect_err(|e| error!("Login DB connection error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    // Try local email+password authentication via service layer
    if !input.disable_local {
        let ctx = ServiceContext::collection(&input.slug, &input.def)
            .conn(&conn)
            .build();

        match authenticate_local(
            &ctx,
            &input.email,
            &input.password,
            &*input.password_provider,
            input.check_verify_email,
        ) {
            Ok(result) => return Ok(Some((result.user, result.session_version))),
            Err(ServiceError::InvalidCredentials)
            | Err(ServiceError::AccountLocked)
            | Err(ServiceError::EmailNotVerified) => {}
            Err(e) => return Err(Status::from(e)),
        }
    }

    // Fallback: try custom auth strategies
    if let Some(auth) = &input.def.auth {
        for strategy in &auth.strategies {
            if let Ok(Some(doc)) = input.hook_runner.run_auth_strategy(
                &strategy.authenticate,
                &input.slug,
                &HashMap::new(),
                &conn,
            ) {
                let ctx = ServiceContext::slug_only(&input.slug).conn(&conn).build();

                // Strategy-authenticated users still need locked/verified checks
                if service::auth::is_locked(&ctx, &doc.id).unwrap_or(false) {
                    return Ok(None);
                }

                if input.check_verify_email
                    && !service::auth::is_verified(&ctx, &doc.id).unwrap_or(false)
                {
                    return Ok(None);
                }

                let sv = service::auth::get_session_version(&ctx, &doc.id).map_err(Status::from)?;
                return Ok(Some((doc, sv)));
            }
        }
    }

    // Equalize timing when all auth methods fail — prevents distinguishing
    // "no valid user" (fast) from "wrong password" (Argon2-slow) via response time.
    if !input.disable_local {
        input.password_provider.dummy_verify();
    }

    Ok(None)
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Authenticate with email/password and return a JWT token.
    pub(in crate::api::handlers) async fn login_impl(
        &self,
        request: Request<content::LoginRequest>,
    ) -> Result<Response<content::LoginResponse>, Status> {
        let ip = request
            .remote_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let req = request.into_inner();

        if self.login_limiter.is_blocked(&req.email) || self.ip_login_limiter.is_blocked(&ip) {
            return Err(Status::resource_exhausted(
                "Too many login attempts. Please try again later.",
            ));
        }

        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection",
                req.collection
            )));
        }

        let disable_local = def.auth.as_ref().is_some_and(|a| a.disable_local);
        let has_strategies = def.auth.as_ref().is_some_and(|a| !a.strategies.is_empty());

        if disable_local && !has_strategies {
            return Err(Status::permission_denied(
                "Local login is disabled for this collection",
            ));
        }

        let input = LoginBlockingInput {
            pool: self.pool.clone(),
            slug: req.collection.clone(),
            email: req.email.clone(),
            password: req.password.clone(),
            def: def.clone(),
            check_verify_email: def.auth.as_ref().is_some_and(|a| a.verify_email),
            disable_local,
            password_provider: self.password_provider.clone(),
            hook_runner: self.hook_runner.clone(),
        };

        let login_result = task::spawn_blocking(move || login_blocking(input))
            .await
            .inspect_err(|e| error!("Login task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        let (user, session_version) = match login_result {
            Some(u) => u,
            None => {
                self.login_limiter.record_failure(&req.email);
                self.ip_login_limiter.record_failure(&ip);
                return Err(Status::unauthenticated("Invalid email or password"));
            }
        };

        let user_email = user
            .fields
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or(&req.email)
            .to_string();

        let expiry = def.auth.as_ref().map(|a| a.token_expiry).unwrap_or(7200);
        let now = Utc::now().timestamp().max(0) as u64;

        let claims = ClaimsBuilder::new(user.id.clone(), Slug::new(&req.collection))
            .email(user_email)
            .exp(now.saturating_add(expiry))
            .auth_time(now)
            .session_version(session_version)
            .build()
            .inspect_err(|e| error!("Claims build error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;

        let token = self
            .token_provider
            .create_token(&claims)
            .inspect_err(|e| error!("Token creation error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;

        self.login_limiter.clear(&req.email);
        self.ip_login_limiter.clear(&ip);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
        }))
    }
}
