//! MFA (email / custom second-factor) code persistence, delivery and
//! verification.
//!
//! A 6-digit code is generated, stored and delivered ([`generate_mfa_code`] /
//! [`deliver_mfa_code`] — built-in email for `mfa = "email"`, the
//! collection's `mfa_deliver` hook for `mfa = "custom"`) by the shared
//! challenge issuance ([`super::challenge::issue_mfa_challenge`]), and
//! verified single-use ([`verify_mfa_code`]).

use chrono::Utc;
use rand::Rng as _;
use tracing::error;

use crate::{
    core::{
        Document,
        collection::{Auth, MfaMode},
        email::{self, MfaCodeEmailContext, SystemEmail},
    },
    db::{
        DbConnection,
        query::{self, MfaCode},
    },
    hooks::lifecycle::MfaDeliverInput,
    service::{AppInfra, ServiceContext, ServiceError, user_settings::recipient_ui_locale},
};

/// MFA pending-token / code lifetime in seconds (5 minutes).
pub const MFA_PENDING_EXPIRY: u64 = 300;

/// Store `mfa`'s code for its user, expiring at `expiry` (Unix timestamp).
///
/// # Errors
///
/// Returns a backend error if the DB connection or persistence
/// fails.
pub fn set_mfa_code(ctx: &ServiceContext, mfa: &MfaCode, expiry: i64) -> Result<(), ServiceError> {
    let conn = ctx.resolve_conn()?;
    query::set_mfa_code(conn.as_ref(), ctx.slug, mfa, expiry)?;

    Ok(())
}

/// Verify an MFA code. Returns true if valid and not expired. The stored
/// code is consumed by the attempt whatever its outcome — atomically, so
/// concurrent attempts on one issued code get exactly one verdict between
/// them (see [`query::verify_mfa_code`]).
///
/// # Errors
///
/// Returns a backend error if the DB connection or query fails.
pub fn verify_mfa_code(ctx: &ServiceContext, attempt: &MfaCode) -> Result<bool, ServiceError> {
    let conn = ctx.resolve_conn()?;

    Ok(query::verify_mfa_code(conn.as_ref(), ctx.slug, attempt)?)
}

/// Generate a fresh 6-digit MFA code.
#[must_use]
pub fn generate_mfa_code() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000))
}

/// One MFA code to store and deliver. Owned: delivery runs detached, in the
/// background.
pub struct MfaCodeDelivery {
    /// `[auth] secret`, which keys the stored code digest.
    pub auth_secret: String,
    /// The auth collection of the pending login.
    pub slug: String,
    /// The user the code is for.
    pub user: Document,
    /// The address a built-in email goes to.
    pub email: String,
    /// The generated code.
    pub code: String,
}

/// Hand the code to the collection's `mfa_deliver` hook (`mfa = "custom"`).
/// Only reachable with a configured hook (startup validation pairs the mode
/// with the hook), but fail LOUDLY if the pairing is somehow broken —
/// silently sending nothing would strand every login on this collection.
///
/// Runs with no connection held: the code is already stored, and the hook's
/// own CRUD takes a write connection only at its first call — so its
/// delivery I/O (an SMS gateway, a push service) never pins one.
fn deliver_custom(infra: &AppInfra, auth: &Auth, d: &MfaCodeDelivery) {
    let Some(hook) = auth.mfa_deliver() else {
        error!(
            collection = d.slug,
            "mfa = \"custom\" without an mfa_deliver hook — no code delivered"
        );
        return;
    };

    let input = MfaDeliverInput {
        collection: &d.slug,
        user: &d.user,
        code: &d.code,
        expires_in: MFA_PENDING_EXPIRY,
    };

    if let Err(e) = infra.hook_runner.run_mfa_deliver(hook, &input, infra) {
        error!(
            collection = d.slug,
            hook = hook.reference(),
            error = ?e,
            "mfa_deliver hook failed — no code delivered"
        );
    }
}

/// Queue the built-in code email (`mfa = "email"`).
fn deliver_email(infra: &AppInfra, conn: &dyn DbConnection, d: &MfaCodeDelivery) {
    let rendered = infra.email.email_renderer.render(
        "mfa_code",
        &MfaCodeEmailContext {
            code: &d.code,
            expiry_minutes: MFA_PENDING_EXPIRY / 60,
            from_name: &infra.email.email_config.from_name,
        },
    );

    let html = match rendered {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render MFA email: {}", e);
            return;
        }
    };

    let locale = recipient_ui_locale(conn, &d.user.id);

    let job = email::EmailJobData {
        to: d.email.clone(),
        subject: infra
            .email
            .email_renderer
            .subject(SystemEmail::MfaCode, &locale),
        html,
        text: None,
    };

    if let Err(e) = email::queue_email(conn, &job, infra.email.email_max_attempts) {
        error!("Failed to queue MFA email: {}", e);
    }
}

/// Store `delivery`'s code on a write connection — and, for the built-in
/// email channel, queue the email with it. `false` when the code could not
/// be stored (logged): nothing may be delivered then.
fn store_mfa_code(infra: &AppInfra, delivery: &MfaCodeDelivery, custom: bool) -> bool {
    let conn = match infra.pool.write() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for MFA code: {}", e);
            return false;
        }
    };

    // Saturate to 0 (immediate expiry) on the impossible overflow path —
    // never-expires is the wrong fallback for security-sensitive timeouts.
    let exp = Utc::now().timestamp() + i64::try_from(MFA_PENDING_EXPIRY).unwrap_or(0);

    let ctx = ServiceContext::slug_only(&delivery.slug)
        .conn(&conn)
        .build();

    let code = MfaCode::builder(&delivery.user.id, &delivery.code, &delivery.auth_secret).build();

    if let Err(e) = set_mfa_code(&ctx, &code, exp) {
        error!("Failed to store MFA code: {}", e);
        return false;
    }

    if !custom {
        deliver_email(infra, &conn, delivery);
    }

    true
}

/// Store a 6-digit MFA code and deliver it — the body the challenge issuance
/// runs in the background. The channel follows the collection's MFA mode:
/// built-in email for `mfa = "email"`, the `mfa_deliver` hook for
/// `mfa = "custom"` (the code is handed to userland for SMS/push/…). The
/// write connection that stores the code is released before a custom hook
/// runs. Best-effort: errors are logged, not propagated — the caller has
/// already committed to the MFA challenge response, and the previously
/// issued code (if any) stays valid.
pub fn deliver_mfa_code(infra: &AppInfra, delivery: &MfaCodeDelivery) {
    let custom_auth = infra
        .registry
        .get_collection(&delivery.slug)
        .and_then(|d| d.auth.as_ref())
        .filter(|auth| auth.mfa() == MfaMode::Custom);

    if !store_mfa_code(infra, delivery, custom_auth.is_some()) {
        return;
    }

    if let Some(auth) = custom_auth {
        deliver_custom(infra, auth, delivery);
    }
}
