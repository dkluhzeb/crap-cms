//! Auth RPCs: login, me, forgot_password, reset_password, verify_email.

use anyhow::Context as _;
use tonic::{Request, Response, Status};

use crate::api::content;
use crate::core::auth;
use crate::core::auth::ClaimsBuilder;
use crate::core::email;
use crate::db::query;

use super::convert::document_to_proto;
use super::ContentService;

/// Untestable as unit: async methods require full ContentService with pool, registry,
/// hook_runner, and JWT secret. Covered by integration tests in tests/grpc_integration.rs.
#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Authenticate with email/password and return a JWT token.
    pub(super) async fn login_impl(
        &self,
        request: Request<content::LoginRequest>,
    ) -> Result<Response<content::LoginResponse>, Status> {
        let req = request.into_inner();

        // Check rate limit before doing any work
        if self.login_limiter.is_blocked(&req.email) {
            return Err(Status::resource_exhausted(
                "Too many login attempts. Please try again later."
            ));
        }

        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection", req.collection
            )));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let email = req.email.clone();
        let password = req.password.clone();
        let def_owned = def.clone();

        let user = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            let doc = query::find_by_email(&conn, &slug, &def_owned, &email)?;
            let doc = match doc {
                Some(d) => d,
                None => { auth::dummy_verify(); return Ok(None); }
            };
            let hash = query::get_password_hash(&conn, &slug, &doc.id)?;
            let hash = match hash {
                Some(h) => h,
                None => { auth::dummy_verify(); return Ok(None); }
            };
            if !auth::verify_password(&password, &hash)? {
                return Ok(None);
            }
            Ok::<_, anyhow::Error>(Some(doc))
        }).await
            .map_err(|e| { tracing::error!("Login task error: {}", e); Status::internal("Internal error") })?
            .map_err(|e| { tracing::error!("Login error: {}", e); Status::internal("Internal error") })?;

        let user = match user {
            Some(u) => u,
            None => {
                self.login_limiter.record_failure(&req.email);
                return Err(Status::unauthenticated("Invalid email or password"));
            }
        };

        // Check if account is locked
        {
            let pool = self.pool.clone();
            let slug = req.collection.clone();
            let uid = user.id.clone();
            let locked = tokio::task::spawn_blocking(move || {
                let conn = pool.get().context("DB connection")?;
                query::is_locked(&conn, &slug, &uid)
            }).await
                .map_err(|e| { tracing::error!("Lock check task error: {}", e); Status::internal("Internal error") })?
                .map_err(|e| { tracing::error!("Lock check error: {}", e); Status::internal("Internal error") })?;

            if locked {
                return Err(Status::permission_denied("This account has been locked"));
            }
        }

        // Check email verification if enabled
        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            let pool = self.pool.clone();
            let slug = req.collection.clone();
            let uid = user.id.clone();
            let verified = tokio::task::spawn_blocking(move || {
                let conn = pool.get().context("DB connection")?;
                query::is_verified(&conn, &slug, &uid)
            }).await
                .map_err(|e| { tracing::error!("Verification check task error: {}", e); Status::internal("Internal error") })?
                .map_err(|e| { tracing::error!("Verification check error: {}", e); Status::internal("Internal error") })?;

            if !verified {
                return Err(Status::permission_denied(
                    "Please verify your email before logging in"
                ));
            }
        }

        let user_email = user.fields.get("email")
            .and_then(|v| v.as_str())
            .unwrap_or(&req.email)
            .to_string();

        let expiry = def.auth.as_ref()
            .map(|a| a.token_expiry)
            .unwrap_or(7200);

        let claims = ClaimsBuilder::new(&user.id, &req.collection)
            .email(user_email)
            .exp((chrono::Utc::now().timestamp() as u64) + expiry)
            .build();

        let token = auth::create_token(&claims, &self.jwt_secret)
            .map_err(|e| { tracing::error!("Token creation error: {}", e); Status::internal("Internal error") })?;

        // Successful login — clear rate limit state
        self.login_limiter.clear(&req.email);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
        }))
    }

    /// Return the currently authenticated user from a JWT token.
    pub(super) async fn me_impl(
        &self,
        request: Request<content::MeRequest>,
    ) -> Result<Response<content::MeResponse>, Status> {
        let req = request.into_inner();

        let claims = auth::validate_token(&req.token, &self.jwt_secret)
            .map_err(|_| Status::unauthenticated("Invalid or expired token"))?;

        let def = self.get_collection_def(&claims.collection)?;

        let pool = self.pool.clone();
        let collection = claims.collection.clone();
        let id = claims.sub.clone();
        let doc = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            query::find_by_id(&conn, &collection, &def, &id, None)
        }).await
            .map_err(|e| { tracing::error!("Me task error: {}", e); Status::internal("Internal error") })?
            .map_err(|e| { tracing::error!("Me query error: {}", e); Status::internal("Internal error") })?;

        let doc = doc.ok_or_else(|| Status::not_found("User not found"))?;

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
        let req = request.into_inner();

        // Rate limit: prevent email flooding (always count, always return success)
        if self.forgot_password_limiter.is_blocked(&req.email) {
            return Ok(Response::new(content::ForgotPasswordResponse { success: true }));
        }
        self.forgot_password_limiter.record_failure(&req.email);

        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection", req.collection
            )));
        }

        if !def.auth.as_ref().is_some_and(|a| a.forgot_password) {
            return Err(Status::permission_denied("Password reset is not enabled for this collection"));
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
                Err(e) => { tracing::error!("DB connection for forgot password: {}", e); return; }
            };

            let user = match query::find_by_email(&conn, &slug, &def_owned, &user_email) {
                Ok(Some(u)) => u,
                Ok(None) => return,
                Err(e) => { tracing::error!("Forgot password lookup: {}", e); return; }
            };

            let token = nanoid::nanoid!();
            let exp = chrono::Utc::now().timestamp() + reset_expiry as i64;

            if let Err(e) = query::set_reset_token(&conn, &slug, &user.id, &token, exp) {
                tracing::error!("Failed to set reset token: {}", e);
                return;
            }

            let base_url = if server_config.host == "0.0.0.0" {
                format!("http://localhost:{}", server_config.admin_port)
            } else {
                format!("http://{}:{}", server_config.host, server_config.admin_port)
            };
            let reset_url = format!("{}/admin/reset-password?token={}", base_url, token);

            let html = match email_renderer.render("password_reset", &serde_json::json!({
                "reset_url": reset_url,
                "expiry_minutes": reset_expiry / 60,
                "from_name": email_config.from_name,
            })) {
                Ok(h) => h,
                Err(e) => { tracing::error!("Failed to render reset email: {}", e); return; }
            };

            if let Err(e) = email::send_email(&email_config, &user_email, "Reset your password", &html, None) {
                tracing::error!("Failed to send reset email: {}", e);
            }
        });

        Ok(Response::new(content::ForgotPasswordResponse { success: true }))
    }

    /// Reset a password using a valid reset token.
    pub(super) async fn reset_password_impl(
        &self,
        request: Request<content::ResetPasswordRequest>,
    ) -> Result<Response<content::ResetPasswordResponse>, Status> {
        let req = request.into_inner();
        let def = self.get_collection_def(&req.collection)?;

        if !def.is_auth_collection() {
            return Err(Status::invalid_argument(format!(
                "Collection '{}' is not an auth collection", req.collection
            )));
        }

        if let Err(e) = self.password_policy.validate(&req.new_password) {
            return Err(Status::invalid_argument(e.to_string()));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let token = req.token.clone();
        let password = req.new_password.clone();
        let def_owned = def;

        tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            let (user, exp) = query::find_by_reset_token(&conn, &slug, &def_owned, &token)?
                .ok_or_else(|| anyhow::anyhow!("Invalid reset token"))?;

            if chrono::Utc::now().timestamp() >= exp {
                query::clear_reset_token(&conn, &slug, &user.id)?;
                return Err(anyhow::anyhow!("Reset token has expired"));
            }

            query::update_password(&conn, &slug, &user.id, &password)?;
            query::clear_reset_token(&conn, &slug, &user.id)?;
            Ok(())
        }).await
            .map_err(|e| { tracing::error!("Reset password task error: {}", e); Status::internal("Internal error") })?
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("Invalid reset token") || msg.contains("expired") {
                    Status::invalid_argument(msg)
                } else {
                    tracing::error!("Reset password error: {}", e);
                    Status::internal("Internal error")
                }
            })?;

        Ok(Response::new(content::ResetPasswordResponse { success: true }))
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
                "Collection '{}' is not an auth collection", req.collection
            )));
        }

        if !def.auth.as_ref().is_some_and(|a| a.verify_email) {
            return Err(Status::invalid_argument("Email verification is not enabled for this collection"));
        }

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let token = req.token.clone();
        let def_owned = def;

        let found = tokio::task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;
            match query::find_by_verification_token(&conn, &slug, &def_owned, &token)? {
                Some((user, exp)) => {
                    if chrono::Utc::now().timestamp() >= exp {
                        return Ok(false); // Token expired
                    }
                    query::mark_verified(&conn, &slug, &user.id)?;
                    Ok(true)
                }
                None => Ok(false),
            }
        }).await
            .map_err(|e| { tracing::error!("Verify email task error: {}", e); Status::internal("Internal error") })?
            .map_err(|e: anyhow::Error| { tracing::error!("Verify email error: {}", e); Status::internal("Internal error") })?;

        if !found {
            return Err(Status::not_found("Invalid verification token"));
        }

        Ok(Response::new(content::VerifyEmailResponse { success: true }))
    }
}
