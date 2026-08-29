//! MFA (email second-factor) code persistence + verification, and the
//! shared challenge-issuance pieces the login surfaces build on.
//!
//! The gRPC `Login`/`VerifyMfa` RPCs and the admin login/MFA pages are
//! codecs over the same primitives: a short-lived pending token
//! ([`mint_mfa_pending_token`]), a 6-digit code stored + delivered
//! ([`generate_mfa_code`] / [`deliver_mfa_code`] — built-in email for
//! `mfa = "email"`, the collection's `mfa_deliver` hook for
//! `mfa = "custom"`), and code verification ([`verify_mfa_code`]). Issuance
//! throttling and the response shape (challenge token vs pending cookie)
//! stay per surface.

use chrono::Utc;
use rand::Rng as _;
use tracing::error;

use crate::{
    core::{
        Document, Slug,
        auth::{ClaimsBuilder, TokenUse},
        collection::{Auth, MfaMode},
        email::{self, MfaCodeEmailContext},
    },
    db::query,
    hooks::lifecycle::MfaDeliverInput,
    service::{AppInfra, ServiceContext, ServiceError},
};

/// MFA pending-token / code lifetime in seconds (5 minutes).
pub const MFA_PENDING_EXPIRY: u64 = 300;

/// Store an MFA code for a user.
///
/// # Errors
///
/// Returns a backend error if the DB connection or persistence
/// fails.
pub fn set_mfa_code(
    ctx: &ServiceContext,
    id: &str,
    code: &str,
    expiry: i64,
) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::set_mfa_code(conn.as_ref(), ctx.slug, id, code, expiry)?;
    Ok(())
}

/// Verify an MFA code. Returns true if valid and not expired.
///
/// # Errors
///
/// Returns a backend error if the DB connection or query fails.
pub fn verify_mfa_code(ctx: &ServiceContext, id: &str, code: &str) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;
    Ok(query::verify_mfa_code(conn.as_ref(), ctx.slug, id, code)?)
}

/// Generate a fresh 6-digit MFA code.
#[must_use]
pub fn generate_mfa_code() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000))
}

/// Mint the short-lived MFA-pending token binding a verified login to its
/// second-factor step. The token carries [`TokenUse::MfaPending`], so it can
/// never pass as a session/bearer token — and a session token can't be
/// replayed into the MFA completion step.
///
/// # Errors
///
/// Returns an error when claims building or token signing fails.
pub fn mint_mfa_pending_token(
    infra: &AppInfra,
    slug: &str,
    user: &Document,
    user_email: &str,
    session_version: u64,
) -> Result<String, ServiceError> {
    let claims = ClaimsBuilder::new(user.id.clone(), Slug::new(slug))
        .email(user_email.to_string())
        .exp((Utc::now().timestamp().max(0).cast_unsigned()).saturating_add(MFA_PENDING_EXPIRY))
        .session_version(session_version)
        .token_use(TokenUse::MfaPending)
        .build()
        .map_err(ServiceError::Internal)?;

    infra
        .token_provider
        .create_token(&claims)
        .map_err(ServiceError::Internal)
}

/// Store a 6-digit MFA code and deliver it — the shared body both login
/// surfaces call (in `spawn_blocking`). The channel follows the collection's
/// MFA mode: built-in email for `mfa = "email"`, the `mfa_deliver` hook for
/// `mfa = "custom"` (the code is handed to userland for SMS/push/…).
/// Best-effort: errors are logged, not propagated — the caller has already
/// committed to the MFA challenge response, and the previously issued code
/// (if any) stays valid.
pub fn deliver_mfa_code(
    infra: &AppInfra,
    slug: &str,
    user: &Document,
    user_email: &str,
    code: &str,
) {
    let conn = match infra.pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for MFA code: {}", e);
            return;
        }
    };

    // Saturate to 0 (immediate expiry) on the impossible overflow path —
    // never-expires is the wrong fallback for security-sensitive timeouts.
    let exp = Utc::now().timestamp() + i64::try_from(MFA_PENDING_EXPIRY).unwrap_or(0);

    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

    if let Err(e) = set_mfa_code(&ctx, &user.id, code, exp) {
        error!("Failed to store MFA code: {}", e);
        return;
    }

    let auth = infra
        .registry
        .get_collection(slug)
        .and_then(|d| d.auth.as_ref());

    // Custom delivery: the hook owns the channel. Only reachable with a
    // configured hook (startup validation pairs `mfa = "custom"` with
    // `mfa_deliver`), but fail LOUDLY if the pairing is somehow broken —
    // silently sending nothing would strand every login on this collection.
    if auth.map(Auth::mfa) == Some(MfaMode::Custom) {
        let Some(hook) = auth.and_then(Auth::mfa_deliver) else {
            error!(
                collection = slug,
                "mfa = \"custom\" without an mfa_deliver hook — no code delivered"
            );
            return;
        };

        let input = MfaDeliverInput {
            collection: slug,
            user,
            code,
            expires_in: MFA_PENDING_EXPIRY,
        };

        if let Err(e) = infra.hook_runner.run_mfa_deliver(hook, &input, &conn) {
            error!(
                collection = slug,
                hook = hook.reference(),
                error = ?e,
                "mfa_deliver hook failed — no code delivered"
            );
        }
        return;
    }

    let html = match infra.email.email_renderer.render(
        "mfa_code",
        &MfaCodeEmailContext {
            code,
            expiry_minutes: MFA_PENDING_EXPIRY / 60,
            from_name: &infra.email.email_config.from_name,
        },
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render MFA email: {}", e);
            return;
        }
    };

    if let Err(e) = email::queue_email(
        &conn,
        &email::EmailJobData {
            to: user_email.to_string(),
            subject: "Your verification code".to_string(),
            html,
            text: None,
        },
        infra.email.email_max_attempts,
    ) {
        error!("Failed to queue MFA email: {}", e);
    }
}
