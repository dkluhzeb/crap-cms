//! Login handler — authenticate with email/password and return a JWT.

use std::collections::HashMap;

use anyhow::{Context as _, Error as AnyhowError};
use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::{
    api::{
        content,
        service::{ContentService, convert::document_to_proto},
    },
    core::{
        Slug,
        auth::{ClaimsBuilder, dummy_verify},
    },
    db::query,
};

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Authenticate with email/password and return a JWT token.
    pub(in crate::api::service) async fn login_impl(
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

        let login_result: Result<Option<_>, &'static str> = task::spawn_blocking(move || {
            let conn = pool.get().context("DB connection")?;

            let mut user = None;

            if !disable_local {
                if let Some(doc) = query::find_by_email(&conn, &slug, &def_owned, &email)? {
                    let verified = match query::get_password_hash(&conn, &slug, &doc.id)? {
                        Some(hash) => {
                            password_provider.verify_password(&password, hash.as_ref())?
                        }
                        None => false,
                    };

                    if verified {
                        user = Some(doc);
                    }
                } else {
                    dummy_verify();
                }
            }

            if user.is_none()
                && let Some(auth) = &def_owned.auth
            {
                for strategy in &auth.strategies {
                    if let Ok(Some(doc)) = hook_runner.run_auth_strategy(
                        &strategy.authenticate,
                        &slug,
                        &HashMap::new(),
                        &conn,
                    ) {
                        user = Some(doc);
                        break;
                    }
                }
            }

            let doc = match user {
                Some(d) => d,
                None => {
                    if !disable_local {
                        dummy_verify();
                    }
                    return Ok(Ok(None));
                }
            };

            if query::is_locked(&conn, &slug, &doc.id)? {
                return Ok(Err("This account has been locked"));
            }

            if check_verify_email && !query::is_verified(&conn, &slug, &doc.id)? {
                return Ok(Err("Please verify your email before logging in"));
            }

            let session_version = query::get_session_version(&conn, &slug, &doc.id)?;

            Ok::<_, AnyhowError>(Ok(Some((doc, session_version))))
        })
        .await
        .map_err(|e| {
            error!("Login task error: {}", e);
            Status::internal("Internal error")
        })?
        .map_err(|e| {
            error!("Login error: {}", e);
            Status::internal("Internal error")
        })?;

        let (user, session_version) = match login_result {
            Ok(Some(u)) => u,
            Ok(None) => {
                self.login_limiter.record_failure(&req.email);
                self.ip_login_limiter.record_failure(&ip);
                return Err(Status::unauthenticated("Invalid email or password"));
            }
            Err(msg) => {
                warn!("Login denied for '{}': {}", req.email, msg);
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
            .exp((Utc::now().timestamp() as u64) + expiry)
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
