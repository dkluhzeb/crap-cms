//! Issuing the MFA challenge — the step a verified authentication takes
//! instead of a session when the collection's MFA gate requires the second
//! factor.
//!
//! The admin login and auth callbacks (pending cookie + redirect to the MFA
//! page) and the gRPC `Login` RPC (challenge token in the response) are codecs
//! over [`issue_mfa_challenge`]: the issuance throttle, the surface-bound
//! pending token, and the code delivery live here once.

use std::sync::Arc;

use chrono::Utc;
use tokio::task;
use tracing::{error, warn};

use crate::{
    core::{
        Builder, Slug,
        auth::{ClaimsBuilder, TokenUse},
        collection::{Auth, MfaMode, Surface},
        rate_limit::{LoginRateLimiter, MFA_ISSUE_KEYSPACE},
    },
    service::{
        AppInfra, ServiceError,
        auth::{
            LoginVerified, MFA_PENDING_EXPIRY, MfaCodeDelivery, deliver_mfa_code, generate_mfa_code,
        },
    },
};

/// One MFA challenge to issue.
#[derive(Builder)]
pub struct ChallengeRequest<'a> {
    /// The auth collection the pending login belongs to.
    #[builder(required)]
    slug: &'a str,
    /// The verified user awaiting the second factor.
    #[builder(required)]
    verified: &'a LoginVerified,
    /// The address the pending token carries and a code is delivered to.
    #[builder(required)]
    email: &'a str,
    /// The surface issuing the challenge — the only one that may complete it.
    #[builder(required)]
    surface: Surface,
    /// `[auth] secret`, which keys the stored code digest.
    #[builder(required)]
    auth_secret: &'a str,
    /// The forgot-password limiter, whose backend the per-user code-issuance
    /// budget is rescoped from.
    #[builder(required)]
    issue_limiter: &'a LoginRateLimiter,
}

/// An issued challenge: the pending token the surface hands back, and the
/// collection's MFA mode (a TOTP surface resolves enrollment itself — admin
/// on the MFA page, gRPC in-band).
pub struct MfaChallenge {
    pub pending_token: String,
    pub mode: MfaMode,
}

/// Why no MFA challenge was issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeRefusal {
    /// The user's code-issuance budget is spent.
    Throttled,
    /// The pending token could not be minted.
    Internal,
}

/// Throttle MFA code ISSUANCE per user (email / custom delivery): `true` when
/// this user's budget is spent. The login limiter is cleared on each
/// successful authentication, so without this a credential-holder could loop
/// the login to flood the victim's inbox. A code is single-use and expires
/// with the pending token, so there is no earlier code to fall back on: over
/// budget, the challenge is refused outright rather than handed out with no
/// code that can complete it.
fn issuance_throttled(req: &ChallengeRequest<'_>) -> bool {
    let user = &req.verified.user.id;

    let throttled = req
        .issue_limiter
        .rescoped(MFA_ISSUE_KEYSPACE)
        .check_and_block(user.as_ref());

    if throttled {
        warn!(user = %user, "MFA code issuance throttled");
    }

    throttled
}

/// Mint the short-lived MFA-pending token binding a verified login to its
/// second-factor step on the issuing surface. The token carries
/// [`TokenUse::MfaPending`], so it can never pass as a session token, and a
/// session token can't be replayed into the MFA completion step.
fn mint_pending_token(
    infra: &AppInfra,
    req: &ChallengeRequest<'_>,
) -> Result<String, ServiceError> {
    let exp = Utc::now()
        .timestamp()
        .max(0)
        .cast_unsigned()
        .saturating_add(MFA_PENDING_EXPIRY);

    let claims = ClaimsBuilder::new(req.verified.user.id.clone(), Slug::new(req.slug))
        .email(req.email)
        .exp(exp)
        .session_version(req.verified.session_version)
        .token_use(TokenUse::MfaPending)
        .surface(req.surface)
        .build()
        .map_err(ServiceError::Internal)?;

    infra
        .token_provider
        .create_token(&claims)
        .map_err(ServiceError::Internal)
}

/// Generate a 6-digit code, then store and deliver it (built-in email or the
/// collection's `mfa_deliver` hook) in the background: the surface answers
/// the challenge without waiting on the delivery channel.
///
/// Detached from the request on purpose (plain `spawn_blocking`, not the
/// request scope): the challenge was already answered, so the delivery's own
/// commit must not be refused by that request's deadline.
fn spawn_code_delivery(infra: &Arc<AppInfra>, req: &ChallengeRequest<'_>) {
    let infra = Arc::clone(infra);
    let delivery = MfaCodeDelivery {
        auth_secret: req.auth_secret.to_string(),
        slug: req.slug.to_string(),
        user: req.verified.user.clone(),
        email: req.email.to_string(),
        code: generate_mfa_code(),
    };

    task::spawn_blocking(move || deliver_mfa_code(&infra, &delivery));
}

/// Issue the MFA challenge for a verified user: throttle code issuance (not
/// for TOTP — nothing is delivered), mint the pending token bound to the
/// issuing surface, and start code delivery for `email` / `custom`.
///
/// Must be called from within the Tokio runtime (delivery is spawned).
///
/// # Errors
///
/// [`ChallengeRefusal`] when no challenge could be issued — the surface
/// answers with its own error.
pub fn issue_mfa_challenge(
    infra: &Arc<AppInfra>,
    req: &ChallengeRequest<'_>,
) -> Result<MfaChallenge, ChallengeRefusal> {
    let mode = infra
        .registry
        .get_collection(req.slug)
        .and_then(|d| d.auth.as_ref())
        .map_or(MfaMode::Off, Auth::mfa);

    if mode != MfaMode::Totp && issuance_throttled(req) {
        return Err(ChallengeRefusal::Throttled);
    }

    let pending_token = mint_pending_token(infra, req)
        .inspect_err(|e| error!("MFA pending token error: {e}"))
        .map_err(|_| ChallengeRefusal::Internal)?;

    if mode != MfaMode::Totp {
        spawn_code_delivery(infra, req);
    }

    Ok(MfaChallenge {
        pending_token,
        mode,
    })
}
