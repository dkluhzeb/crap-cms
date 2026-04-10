//! Login handler — authenticate with email/password and return a JWT.

use std::collections::HashMap;

use anyhow::{Context as _, Error as AnyhowError};
use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{ContentService, convert::document_to_proto},
    },
    core::{Slug, auth::ClaimsBuilder},
    service::{self, ServiceError, auth::authenticate_local},
};

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

        let pool = self.pool.clone();
        let slug = req.collection.clone();
        let email = req.email.clone();
        let password = req.password.clone();
        let def_owned = def.clone();
        let check_verify_email = def.auth.as_ref().is_some_and(|a| a.verify_email);
        let password_provider = self.password_provider.clone();
        let hook_runner = self.hook_runner.clone();

        let login_result = task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;

            // Try local email+password authentication via service layer
            if !disable_local {
                match authenticate_local(
                    &conn,
                    &slug,
                    &def_owned,
                    &email,
                    &password,
                    &*password_provider,
                    check_verify_email,
                ) {
                    Ok(result) => return Ok(Some((result.user, result.session_version))),
                    Err(ServiceError::InvalidCredentials)
                    | Err(ServiceError::AccountLocked)
                    | Err(ServiceError::EmailNotVerified) => {}
                    Err(e) => return Err(e.into_anyhow()),
                }
            }

            // Fallback: try custom auth strategies
            if let Some(auth) = &def_owned.auth {
                for strategy in &auth.strategies {
                    if let Ok(Some(doc)) = hook_runner.run_auth_strategy(
                        &strategy.authenticate,
                        &slug,
                        &HashMap::new(),
                        &conn,
                    ) {
                        // Strategy-authenticated users still need locked/verified checks
                        if service::auth::is_locked(&conn, &slug, &doc.id).unwrap_or(false) {
                            return Ok(None);
                        }

                        if check_verify_email
                            && !service::auth::is_verified(&conn, &slug, &doc.id).unwrap_or(false)
                        {
                            return Ok(None);
                        }

                        let sv = service::auth::get_session_version(&conn, &slug, &doc.id)
                            .map_err(|e| e.into_anyhow())?;
                        return Ok(Some((doc, sv)));
                    }
                }
            }

            // Equalize timing when all auth methods fail — prevents distinguishing
            // "no valid user" (fast) from "wrong password" (Argon2-slow) via response time.
            if !disable_local {
                password_provider.dummy_verify();
            }

            Ok::<_, AnyhowError>(None)
        })
        .await
        .inspect_err(|e| error!("Login task error: {}", e))
        .map_err(|_| Status::internal("Internal error"))?
        .map_err(|e| {
            error!("Login error: {}", e);
            Status::internal("Internal error")
        })?;

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

        let claims = ClaimsBuilder::new(user.id.clone(), Slug::new(&req.collection))
            .email(user_email)
            .exp((Utc::now().timestamp().max(0) as u64).saturating_add(expiry))
            .session_version(session_version)
            .build()
            .map_err(|e| {
                error!("Claims build error: {}", e);
                Status::internal("Internal error")
            })?;

        let token = self.token_provider.create_token(&claims).map_err(|e| {
            error!("Token creation error: {}", e);
            Status::internal("Internal error")
        })?;

        self.login_limiter.clear(&req.email);
        self.ip_login_limiter.clear(&ip);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
        }))
    }
}
