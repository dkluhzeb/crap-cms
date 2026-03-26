//! Auth RPCs: login, me, forgot_password, reset_password, verify_email.

use anyhow::Context as _;
use serde_json::json;
use tonic::{Request, Response, Status};

use crate::{
    api::content,
    core::{
        Slug,
        auth::{self, ClaimsBuilder, ResetTokenError},
        email,
    },
    db::query,
};

use super::{ContentService, convert::document_to_proto};

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/grpc_integration.rs.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Authenticate with email/password and return a JWT token.
    pub(super) async fn login_impl(
        &self,
        request: Request<content::LoginRequest>,
    ) -> Result<Response<content::LoginResponse>, Status> {
        let ip = request
            .remote_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let req = request.into_inner();

        // Check rate limit before doing any work
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

        if def.auth.as_ref().is_some_and(|a| a.disable_local) {
            return Err(Status::permission_denied(
                "Local login is disabled for this collection",
            ));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let email = req.email.clone();
        let password = req.password.clone();
        let def_owned = def.clone();
        let check_verify_email = def.auth.as_ref().is_some_and(|a| a.verify_email);

        // All checks in a single spawn_blocking: find user → verify password →
        // check locked → check email verified. This ordering prevents locked/unverified
        // accounts from leaking whether the password was correct.
        let login_result: Result<Option<_>, &'static str> =
            tokio::task::spawn_blocking(move || {
                let conn = pool.get().context("DB connection")?;
                let doc = query::find_by_email(&conn, &slug, &def_owned, &email)?;
                let doc = match doc {
                    Some(d) => d,
                    None => {
                        auth::dummy_verify();
                        return Ok(Ok(None));
                    }
                };
                let hash = query::get_password_hash(&conn, &slug, &doc.id)?;
                let hash = match hash {
                    Some(h) => h,
                    None => {
                        auth::dummy_verify();
                        return Ok(Ok(None));
                    }
                };

                if !auth::verify_password(&password, hash.as_ref())? {
                    return Ok(Ok(None));
                }

                // Password is correct — now check locked and email verification
                if query::is_locked(&conn, &slug, &doc.id)? {
                    return Ok(Err("This account has been locked"));
                }
                if check_verify_email && !query::is_verified(&conn, &slug, &doc.id)? {
                    return Ok(Err("Please verify your email before logging in"));
                }

                Ok::<_, anyhow::Error>(Ok(Some(doc)))
            })
            .await
            .map_err(|e| {
                tracing::error!("Login task error: {}", e);
                Status::internal("Internal error")
            })?
            .map_err(|e| {
                tracing::error!("Login error: {}", e);
                Status::internal("Internal error")
            })?;

        let user = match login_result {
            Ok(Some(u)) => u,
            Ok(None) => {
                self.login_limiter.record_failure(&req.email);
                self.ip_login_limiter.record_failure(&ip);
                return Err(Status::unauthenticated("Invalid email or password"));
            }
            Err(msg) => {
                // Log the actual reason for observability, but return the same
                // generic error as wrong-password to prevent attackers from
                // confirming password correctness on locked/unverified accounts.
                tracing::warn!("Login denied for '{}': {}", req.email, msg);
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

        let claims = ClaimsBuilder::new(user.id.clone(), Slug::new(&req.collection))
            .email(user_email)
            .exp((chrono::Utc::now().timestamp() as u64) + expiry)
            .build();

        let token = auth::create_token(&claims, self.jwt_secret.as_ref()).map_err(|e| {
            tracing::error!("Token creation error: {}", e);
            Status::internal("Internal error")
        })?;

        // Successful login — clear rate limit state for both email and IP
        self.login_limiter.clear(&req.email);
        self.ip_login_limiter.clear(&ip);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
        }))
    }

    /// Return the currently authenticated user from a JWT token.
    /// Checks metadata `authorization` header first, falls back to body `token` field.
    pub(super) async fn me_impl(
        &self,
        request: Request<content::MeRequest>,
    ) -> Result<Response<content::MeResponse>, Status> {
        let metadata = request.metadata().clone();
        let req = request.into_inner();
        let token = Self::extract_token(&metadata)
            .or_else(|| {
                let t = &req.token;
                if t.is_empty() { None } else { Some(t.clone()) }
            })
            .ok_or_else(|| Status::unauthenticated("Missing token"))?;

        let claims = auth::validate_token(&token, self.jwt_secret.as_ref())
            .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;

        let def = self.get_collection_def(&claims.collection)?;

        let pool = self.pool.clone();
        let collection = claims.collection.clone();
        let id = claims.sub.clone();
        let doc = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            query::find_by_id(&conn, &collection, &def, &id, None)
        })
        .await
        .map_err(|e| {
            tracing::error!("Me task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| {
            tracing::error!("Me query error: {}", e);
            Status::internal("Internal error")
        })?;

        let doc = doc.ok_or_else(|| Status::not_found("User not found"))?;

        // Reject locked users even if their JWT is still valid
        let locked = doc
            .fields
            .get("_locked")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            != 0;
        if locked {
            return Err(Status::unauthenticated("Account is locked"));
        }

        Ok(Response::new(content::MeResponse {
            user: Some(document_to_proto(&doc, &claims.collection)),
        }))
    }

    /// Initiate a password reset flow -- generates a token and sends a reset email.
    /// Always returns success to prevent leaking user existence.
    pub(super) async fn forgot_password_impl(
        &self,
        request: Request<content::ForgotPasswordRequest>,
    ) -> Result<Response<content::ForgotPasswordResponse>, Status> {
        let ip = request
            .remote_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let req = request.into_inner();

        // Rate limit: prevent email/IP flooding
        if self.forgot_password_limiter.is_blocked(&req.email)
            || self.ip_forgot_password_limiter.is_blocked(&ip)
        {
            return Ok(Response::new(content::ForgotPasswordResponse {
                success: true,
            }));
        }

        // Record rate limit immediately to prevent concurrent request bypass.
        // Safe to record unconditionally — the response is always "success"
        // regardless of whether the email exists, so no information is leaked.
        self.forgot_password_limiter.record_failure(&req.email);
        self.ip_forgot_password_limiter.record_failure(&ip);

        let ok_response = Response::new(content::ForgotPasswordResponse { success: true });

        let def = match self.get_collection_def(&req.collection) {
            Ok(d) => d,
            // Return success for non-existent collections to prevent leaking validity
            Err(_) => return Ok(ok_response),
        };

        if !def.is_auth_collection()
            || !def.auth.as_ref().is_some_and(|a| a.forgot_password)
            || def.auth.as_ref().is_some_and(|a| a.disable_local)
        {
            // Return success to prevent leaking collection configuration
            return Ok(ok_response);
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let user_email = req.email.clone();
        let def_owned = def;
        let email_config = self.email_config.clone();
        let email_renderer = self.email_renderer.clone();
        let server_config = self.server_config.clone();
        let reset_expiry = self.reset_token_expiry;
        // Fire and forget -- always return success
        tokio::task::spawn_blocking(move || {
            let conn = match pool.get() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("DB connection for forgot password: {}", e);

                    return;
                }
            };

            let user = match query::find_by_email(&conn, &slug, &def_owned, &user_email) {
                Ok(Some(u)) => u,
                Ok(None) => return,
                Err(e) => {
                    tracing::error!("Forgot password lookup: {}", e);

                    return;
                }
            };

            let token = nanoid::nanoid!();
            let exp = chrono::Utc::now().timestamp() + reset_expiry as i64;

            if let Err(e) = query::set_reset_token(&conn, &slug, &user.id, &token, exp) {
                tracing::error!("Failed to set reset token: {}", e);

                return;
            }

            let base_url = server_config.public_url.clone().unwrap_or_else(|| {
                if server_config.host == "0.0.0.0" {
                    format!("http://localhost:{}", server_config.admin_port)
                } else {
                    format!("http://{}:{}", server_config.host, server_config.admin_port)
                }
            });
            let reset_url = format!("{}/admin/reset-password?token={}", base_url, token);

            let html = match email_renderer.render(
                "password_reset",
                &json!({
                    "reset_url": reset_url,
                    "expiry_minutes": reset_expiry / 60,
                    "from_name": email_config.from_name,
                }),
            ) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("Failed to render reset email: {}", e);

                    return;
                }
            };

            if let Err(e) = email::send_email(
                &email_config,
                &user_email,
                "Reset your password",
                &html,
                None,
            ) {
                tracing::error!("Failed to send reset email: {}", e);
            }
        });

        Ok(Response::new(content::ForgotPasswordResponse {
            success: true,
        }))
    }

    /// Reset a password using a valid reset token.
    pub(super) async fn reset_password_impl(
        &self,
        request: Request<content::ResetPasswordRequest>,
    ) -> Result<Response<content::ResetPasswordResponse>, Status> {
        let ip = request
            .remote_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let req = request.into_inner();

        // Rate limit by IP — prevents brute-forcing reset tokens
        if self.ip_login_limiter.is_blocked(&ip) {
            return Err(Status::resource_exhausted(
                "Too many attempts, try again later",
            ));
        }

        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection",
                req.collection
            )));
        }

        if def.auth.as_ref().is_some_and(|a| a.disable_local) {
            return Err(Status::permission_denied(
                "Local login is disabled for this collection",
            ));
        }

        if let Err(e) = self.password_policy.validate(&req.new_password) {
            return Err(Status::invalid_argument(e.to_string()));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let token = req.token.clone();
        let password = req.new_password.clone();
        let def_owned = def;

        let result: Result<(), anyhow::Error> = tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;
            let (user, exp) = query::find_by_reset_token(&tx, &slug, &def_owned, &token)?
                .ok_or(ResetTokenError::NotFound)?;

            if chrono::Utc::now().timestamp() >= exp {
                query::clear_reset_token(&tx, &slug, &user.id)?;
                tx.commit()?;

                return Err(ResetTokenError::Expired.into());
            }

            query::update_password(&tx, &slug, &user.id, &password)?;
            query::clear_reset_token(&tx, &slug, &user.id)?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| {
            tracing::error!("Reset password task error: {}", e);
            Status::internal("Internal error")
        })?;

        match result {
            Ok(()) => Ok(Response::new(content::ResetPasswordResponse {
                success: true,
            })),
            Err(e) => {
                // Record failure on invalid/expired token — not on success
                self.ip_login_limiter.record_failure(&ip);

                match e.downcast_ref::<ResetTokenError>() {
                    Some(_) => Err(Status::invalid_argument(e.to_string())),
                    None => {
                        tracing::error!("Reset password error: {}", e);
                        Err(Status::internal("Internal error"))
                    }
                }
            }
        }
    }

    /// Verify an email address using a verification token.
    pub(super) async fn verify_email_impl(
        &self,
        request: Request<content::VerifyEmailRequest>,
    ) -> Result<Response<content::VerifyEmailResponse>, Status> {
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection",
                req.collection
            )));
        }

        if !def.auth.as_ref().is_some_and(|a| a.verify_email) {
            return Err(Status::invalid_argument(
                "Email verification is not enabled for this collection",
            ));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let token = req.token.clone();
        let def_owned = def;

        let found = tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().context("DB connection")?;
            let tx = conn.transaction().context("Start transaction")?;
            match query::find_by_verification_token(&tx, &slug, &def_owned, &token)? {
                Some((user, exp)) => {
                    if chrono::Utc::now().timestamp() >= exp {
                        // Clean up expired token
                        if let Err(e) = query::clear_verification_token(&tx, &slug, &user.id) {
                            tracing::warn!("Failed to clear expired verification token: {}", e);
                        }
                        tx.commit()?;
                        return Ok(false); // Token expired
                    }
                    query::mark_verified(&tx, &slug, &user.id)?;
                    tx.commit()?;
                    Ok(true)
                }
                None => Ok(false),
            }
        })
        .await
        .map_err(|e| {
            tracing::error!("Verify email task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e: anyhow::Error| {
            tracing::error!("Verify email error: {}", e);
            Status::internal("Internal error")
        })?;

        if !found {
            return Err(Status::not_found("Invalid verification token"));
        }

        Ok(Response::new(content::VerifyEmailResponse {
            success: true,
        }))
    }
}
