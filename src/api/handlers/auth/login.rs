//! Login handler — authenticate with email/password and return a JWT.
//!
//! Codec over [`service::auth::verify_login`] — the credential flow (local
//! password auth, strategy fallback, locked/verified checks, timing
//! equalization, MFA gate) is shared with the admin login. This surface owns
//! rate limiting and the JWT response shape.

use std::{collections::HashMap, sync::Arc};

use tokio::task;
use tonic::{Request, Response, Status};
use tracing::error;

use crate::{
    api::{
        content,
        handlers::{
            ContentService, auth::user_response::prepare_user_document,
            content_service::pool_error_status, proto::document_to_proto,
        },
        request_client_ip,
    },
    core::{
        CollectionDefinition, Document, SharedPasswordProvider,
        collection::{Auth, MfaMode, Surface},
        login_email_key,
        rate_limit::AttemptBudget,
    },
    service::{
        AppInfra,
        auth::{
            self, ChallengeRefusal, ChallengeRequest, LoginFlowRequest, LoginOutcome,
            LoginVerified, SessionGrant, mint_session, verify_login,
        },
    },
};

/// Owned bundle for the login spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct LoginBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    email: String,
    password: String,
    def: Arc<CollectionDefinition>,
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
    .map_err(|e| Status::from(e.reclassify(input.infra.pool.kind())))
}

#[cfg(not(tarpaulin_include))]
impl ContentService {
    /// Hydrate + strip a freshly authenticated user document for the wire.
    ///
    /// Runs on a blocking thread: the strip evaluates field `access.read`
    /// hooks against a pooled connection.
    pub(super) async fn prepare_login_user(
        &self,
        collection: &str,
        user: Document,
    ) -> Result<Document, Status> {
        let infra = Arc::clone(&self.infra);
        let collection = collection.to_string();

        task::spawn_blocking(move || -> Result<Document, Status> {
            let Some(def) = infra.registry.get_collection(&collection).cloned() else {
                return Err(Status::unauthenticated("Auth collection no longer exists"));
            };
            let conn = infra
                .pool
                .get()
                .inspect_err(|e| error!("Login response pool error: {e}"))
                .map_err(|e| pool_error_status(e, infra.pool.kind()))?;

            // The same mapping an ordinary read of this collection gets, so a
            // `before_read` abort reports its message as INVALID_ARGUMENT here
            // too instead of an opaque INTERNAL the client retries on.
            let mut user = user;
            prepare_user_document(&infra, &def, &collection, &mut user, &conn)
                .inspect_err(|e| error!("Login response user read error for {collection}: {e}"))
                .map_err(|e| Status::from(e.reclassify(infra.pool.kind())))?;

            Ok(user)
        })
        .await
        .inspect_err(|e| error!("Login response task error: {e}"))
        .map_err(|_| Status::internal("Internal error"))?
    }

    /// Authenticate with email/password and return a JWT token.
    pub(in crate::api::handlers) async fn login_impl(
        &self,
        request: Request<content::LoginRequest>,
    ) -> Result<Response<content::LoginResponse>, Status> {
        let client = request_client_ip(&request, &self.server_config);
        let headers = self.metadata_headers(request.metadata());
        let req = request.into_inner();

        // An address longer than any deliverable one cannot name an account:
        // it is refused before it is keyed, throttled or looked up. Otherwise
        // the per-email limiter is keyed on the address in its stored form
        // (trimmed, lowercased, NFC-composed) — the form `find_by_email`
        // compares — so spelling variants of one account share a bucket.
        // Mirrors the admin login twin (`login_action.rs`).
        let Some(email_key) = login_email_key(&req.email) else {
            return Err(Status::unauthenticated("Invalid email or password"));
        };

        // Atomically record this attempt, IP budget first (see
        // `AttemptBudget`), and reject if either budget is spent.
        let budget = AttemptBudget::new(&self.ip_login_limiter, &self.login_limiter);
        if budget.check_and_block(&client, &email_key) {
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
            remote_addr: client.to_string(),
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
            // The password is proven, so settle the login limiters exactly as
            // a completed login and the admin twin do: clear the per-email
            // counter, refund the shared per-IP attempt. Code issuance has its
            // own per-user limiter (see `issue_mfa_challenge`).
            LoginOutcome::MfaRequired(v) => {
                budget.settle_success(&client, &email_key);

                return self
                    .issue_mfa_challenge(&req.collection, &req.email, &v)
                    .await;
            }
            LoginOutcome::Denied => {
                // Attempt already recorded up front by check_and_block.
                return Err(Status::unauthenticated("Invalid email or password"));
            }
        };

        // The credential lookup is a raw row read; give the response document
        // the same shape and stripping every other `Document` on the wire has.
        let user = self
            .prepare_login_user(&req.collection, verified.user)
            .await?;
        let user_email = user
            .fields
            .get("email")
            .and_then(|v| v.as_str())
            .unwrap_or(&req.email)
            .to_string();

        // gRPC tokens are never refreshed, so no absolute ceiling applies:
        // the token's lifetime is the collection's `token_expiry` (or the
        // global `[auth] token_expiry` when it sets none).
        let grant = SessionGrant::builder(
            &user.id,
            &req.collection,
            &user_email,
            verified.session_version,
            Surface::Grpc,
        )
        .mfa(verified.mfa)
        .build();

        let token = mint_session(&self.infra, &grant)
            .inspect_err(|e| error!("Session mint error: {e}"))
            .map_err(|_| Status::internal("Internal error"))?
            .token;

        // Clear the per-email budget (this account just proved its identity)
        // and only REFUND the shared per-IP attempt: a success must not wipe
        // other accounts' failures from the same IP — that would let one valid
        // account on a shared IP mask a brute-force of others.
        budget.settle_success(&client, &email_key);

        Ok(Response::new(content::LoginResponse {
            token,
            user: Some(document_to_proto(&user, &req.collection)),
            mfa_required: None,
            mfa_challenge: None,
            totp_provisioning_uri: None,
        }))
    }

    /// Resolve the TOTP enrollment state for an issued TOTP challenge, so an
    /// unconfirmed user receives the provisioning URI in-band.
    async fn totp_provisioning_uri(
        &self,
        collection: &str,
        user: &Document,
    ) -> Result<Option<String>, Status> {
        let infra = Arc::clone(&self.infra);
        let secret = self.auth_secret.clone();
        let slug = collection.to_string();
        let user = user.clone();

        let provisioning =
            task::spawn_blocking(move || auth::totp_challenge(&infra, &secret, &slug, &user))
                .await
                .inspect_err(|e| error!("TOTP challenge task error: {e}"))
                .map_err(|_| Status::internal("Internal error"))?
                .inspect_err(|e| error!("TOTP challenge error: {e:?}"))
                .map_err(|_| Status::internal("Internal error"))?;

        Ok(provisioning.map(|p| p.uri))
    }

    /// Issue the MFA challenge for a credential-verified but MFA-gated login
    /// through the shared service chokepoint, and encode it as the challenge
    /// response (no session token). The pending token is bound to the gRPC
    /// surface, so only `VerifyMfa` can complete it.
    async fn issue_mfa_challenge(
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
            .unwrap_or(fallback_email);

        let request = ChallengeRequest::builder(
            collection,
            verified,
            user_email,
            Surface::Grpc,
            &self.auth_secret,
            &self.forgot_password_limiter,
        )
        .build();

        let challenge =
            auth::issue_mfa_challenge(&self.infra, &request).map_err(|refusal| match refusal {
                ChallengeRefusal::Throttled => Status::resource_exhausted(
                    "Too many verification codes requested. Please try again later.",
                ),
                ChallengeRefusal::Internal => Status::internal("Internal error"),
            })?;

        let totp_provisioning_uri = if challenge.mode == MfaMode::Totp {
            self.totp_provisioning_uri(collection, &verified.user)
                .await?
        } else {
            None
        };

        Ok(Response::new(content::LoginResponse {
            token: String::new(),
            user: None,
            mfa_required: Some(true),
            mfa_challenge: Some(challenge.pending_token),
            totp_provisioning_uri,
        }))
    }
}
