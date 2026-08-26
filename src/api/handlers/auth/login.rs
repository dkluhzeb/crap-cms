//! Login handler — authenticate with email/password and return a JWT.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::{
        CollectionDefinition, Document, SharedPasswordProvider, Slug, auth::ClaimsBuilder,
        normalize_email,
    },
    hooks::lifecycle::AuthStrategyInput,
    service::{self, AppInfra, ServiceContext, ServiceError, auth::authenticate_local},
};

/// Owned bundle for the `Login` spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct LoginBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    email: String,
    password: String,
    def: CollectionDefinition,
    check_verify_email: bool,
    allows_password: bool,
    password_provider: SharedPasswordProvider,
    headers: HashMap<String, String>,
    remote_addr: String,
}

/// Try local email+password auth first, then any configured custom strategies.
/// Returns `Ok(Some((user, session_version)))` on success, `Ok(None)` for any
/// recoverable failure (wrong password, locked, unverified). Errors propagate
/// only for system failures.
fn login_blocking(input: &LoginBlockingInput) -> Result<Option<(Document, u64)>, Status> {
    // Local auth reads the user then writes lockout/attempt counters on this
    // same connection, so it is write-capable and draws from the write pool.
    let conn = input
        .infra
        .pool
        .write()
        .inspect_err(|e| error!("Login DB connection error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?;

    // Try local email+password authentication via service layer
    if input.allows_password {
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
            Err(
                ServiceError::InvalidCredentials
                | ServiceError::AccountLocked
                | ServiceError::EmailNotVerified,
            ) => {}
            Err(e) => return Err(Status::from(e)),
        }
    }

    // Fallback: try custom auth strategies
    if let Some(auth) = &input.def.auth {
        let strategy_input = AuthStrategyInput {
            collection: &input.slug,
            headers: &input.headers,
            email: Some(&input.email),
            password: Some(&input.password),
            remote_addr: Some(&input.remote_addr),
        };

        for strategy in auth.strategies() {
            if let Ok(Some(doc)) = input.infra.hook_runner.run_auth_strategy(
                strategy.authenticate,
                &strategy_input,
                &conn,
            ) {
                let ctx = ServiceContext::slug_only(&input.slug).conn(&conn).build();

                // Strategy-authenticated users still need locked/verified
                // checks. A lookup failure must fail CLOSED (deny) — letting
                // a locked account in on a transient DB error is an auth
                // bypass. Matches the `get_session_version` call below.
                if service::auth::is_locked(&ctx, &doc.id).map_err(Status::from)? {
                    return Ok(None);
                }

                if input.check_verify_email
                    && !service::auth::is_verified(&ctx, &doc.id).map_err(Status::from)?
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
    if input.allows_password {
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
            .map_or_else(|| "unknown".to_string(), |a| a.ip().to_string());
        let headers = self.metadata_headers(request.metadata());
        let req = request.into_inner();

        // Normalize the per-email limiter key (trim + lowercase). `find_by_email`
        // is case-insensitive (`LOWER(email)=LOWER(?)`), so keying the limiter on
        // the raw address would let an attacker rotate casing/whitespace to get a
        // fresh lockout bucket per spelling of one account. Mirrors the admin
        // login twin (`login_action.rs`).
        let email_key = normalize_email(&req.email);

        // Atomically record this attempt against both limiters and reject if
        // either is now over threshold — one operation per limiter, closing the
        // burst race the old is_blocked + later record_failure split left open.
        // Both are evaluated (not short-circuited) so each counter advances;
        // a successful login clears both below.
        let email_blocked = self.login_limiter.check_and_block(&email_key);
        let ip_blocked = self.ip_login_limiter.check_and_block(&ip);
        if email_blocked || ip_blocked {
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

        let allows_password = def.auth.as_ref().is_some_and(Auth::password_login_enabled);
        let has_strategies = def.auth.as_ref().is_some_and(Auth::has_strategies);

        if !allows_password && !has_strategies {
            return Err(Status::permission_denied(
                "Local login is disabled for this collection",
            ));
        }

        let input = LoginBlockingInput {
            infra: Arc::clone(&self.infra),
            slug: req.collection.clone(),
            email: req.email.clone(),
            password: req.password.clone(),
            def: def.clone(),
            check_verify_email: def.auth.as_ref().is_some_and(Auth::requires_verify_email),
            allows_password,
            password_provider: self.password_provider.clone(),
            headers,
            remote_addr: ip.clone(),
        };

        let login_result = task::spawn_blocking(move || login_blocking(&input))
            .await
            .inspect_err(|e| error!("Login task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        let Some((user, session_version)) = login_result else {
            // Attempt already recorded up front by check_and_block.
            return Err(Status::unauthenticated("Invalid email or password"));
        };

        let user_email = user
            .fields
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or(&req.email)
            .to_string();

        let expiry = def.auth.as_ref().map_or(7200, |a| a.token_expiry);
        let now = Utc::now().timestamp().max(0).cast_unsigned();

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

        self.login_limiter.clear(&email_key);
        self.ip_login_limiter.clear(&ip);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
        }))
    }
}
