//! Login handler — authenticate with email/password and return a JWT.
//!
//! Codec over [`service::auth::verify_login`] — the credential flow (local
//! password auth, strategy fallback, locked/verified checks, timing
//! equalization, MFA gate) is shared with the admin login. This surface owns
//! rate limiting and the JWT response shape.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::core::collection::{Auth, Surface};
use crate::{
    api::{
        content,
        handlers::{ContentService, proto::document_to_proto},
    },
    core::{
        CollectionDefinition, SharedPasswordProvider, Slug, auth::ClaimsBuilder, normalize_email,
    },
    service::{
        AppInfra,
        auth::{self, LoginFlowRequest, LoginOutcome, LoginVerified, verify_login},
    },
};

/// Owned bundle for the login spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct LoginBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    email: String,
    password: String,
    def: CollectionDefinition,
    password_provider: SharedPasswordProvider,
    headers: HashMap<String, String>,
    remote_addr: String,
}

fn login_blocking(input: &LoginBlockingInput) -> Result<LoginOutcome, Status> {
    verify_login(
        &input.infra,
        &LoginFlowRequest {
            slug: &input.slug,
            def: &input.def,
            email: &input.email,
            password: &input.password,
            headers: &input.headers,
            remote_addr: Some(&input.remote_addr),
            surface: Surface::Grpc,
            password_provider: &*input.password_provider,
        },
    )
    .map_err(Status::from)
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
        // Both are evaluated (not short-circuited) so each counter advances.
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
            password_provider: self.password_provider.clone(),
            headers,
            remote_addr: ip.clone(),
        };

        let outcome = task::spawn_blocking(move || login_blocking(&input))
            .await
            .inspect_err(|e| error!("Login task error: {}", e))
            .map_err(|_| Status::internal("Internal error"))??;

        let verified = match outcome {
            LoginOutcome::Verified(v) => v,
            // The collection requires a second factor: issue the challenge —
            // store + email a 6-digit code, mint the short-lived pending
            // token — and return it WITHOUT a session token. The client
            // completes the login via the VerifyMfa RPC.
            //
            // The login limiters are deliberately NOT cleared here: each
            // code-issuing Login costs one attempt, which caps how fast a
            // password-holder can flood the victim's inbox with codes (the
            // admin twin uses a dedicated issuance limiter because it clears
            // the login limiter on password success; this surface doesn't).
            LoginOutcome::MfaRequired(v) => {
                return self.issue_mfa_challenge(&req.collection, &req.email, &v);
            }
            LoginOutcome::Denied => {
                // Attempt already recorded up front by check_and_block.
                return Err(Status::unauthenticated("Invalid email or password"));
            }
        };

        let user = verified.user;
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
            .session_version(verified.session_version)
            .build()
            .inspect_err(|e| error!("Claims build error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;

        let token = self
            .infra
            .token_provider
            .create_token(&claims)
            .inspect_err(|e| error!("Token creation error: {}", e))
            .map_err(|_| Status::internal("Internal error"))?;

        // Clear the per-email limiter (this account just proved its
        // identity). For the SHARED per-IP limiter, only REFUND this one
        // attempt: a success must not wipe other accounts' failures from the
        // same IP — that would let one valid account on a shared IP mask a
        // brute-force of others. Mirrors the admin login (which got this fix
        // first; the gRPC twin had kept the full clear).
        self.login_limiter.clear(&email_key);
        self.ip_login_limiter.refund(&ip);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
            mfa_required: None,
            mfa_challenge: None,
        }))
    }

    /// Issue the MFA challenge for a credential-verified but MFA-gated login:
    /// mint the pending token, store + email a fresh 6-digit code (background,
    /// best-effort), and encode the challenge response (no session token).
    fn issue_mfa_challenge(
        &self,
        collection: &str,
        fallback_email: &str,
        verified: &LoginVerified,
    ) -> Result<Response<content::LoginResponse>, Status> {
        let user_email = verified
            .user
            .fields
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or(fallback_email)
            .to_string();

        let mfa_challenge = auth::mint_mfa_pending_token(
            &self.infra,
            collection,
            &verified.user,
            &user_email,
            verified.session_version,
        )
        .inspect_err(|e| error!("MFA pending token error: {e}"))
        .map_err(|_| Status::internal("Internal error"))?;

        let code = auth::generate_mfa_code();
        let infra = Arc::clone(&self.infra);
        let slug = collection.to_string();
        let user_owned = verified.user.clone();

        task::spawn_blocking(move || {
            auth::deliver_mfa_code(&infra, &slug, &user_owned, &user_email, &code);
        });

        Ok(Response::new(content::LoginResponse {
            token: String::new(),
            user: None,
            mfa_required: Some(true),
            mfa_challenge: Some(mfa_challenge),
        }))
    }
}
