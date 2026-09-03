//! TOTP challenge + verification flow — the service-level counterpart of
//! [`super::mfa`] for `mfa = "totp"`.
//!
//! Enrollment is challenge-driven: the first MFA challenge generates and
//! seals a secret; the provisioning URI is returned (and re-returned) until
//! the first successful verification confirms enrollment. Verification is
//! replay-guarded via the persisted last accepted time step.

use chrono::Utc;
use tracing::{error, info};

use crate::{
    core::{
        Document,
        auth::{
            generate_totp_secret, open_totp_secret, provisioning_uri, seal_totp_secret, verify_totp,
        },
        collection::{Auth, MfaMode},
    },
    db::query,
    service::{AppInfra, ServiceContext, ServiceError},
};

/// The enrollment material shown while a user's TOTP is unconfirmed.
pub struct TotpProvisioning {
    /// `otpauth://` URI (QR payload / tap-to-add link).
    pub uri: String,
    /// The base32 secret, for manual entry.
    pub secret: String,
}

/// Prepare the TOTP side of an MFA challenge for `user`. Returns the
/// provisioning material while enrollment is unconfirmed, `None` once
/// confirmed (the user just enters their authenticator code).
///
/// Generates + seals a fresh secret when none exists — and also when the
/// stored one no longer opens (a rotated `[auth] secret`): an unconfirmed
/// or unopenable enrollment restarts rather than stranding the login.
///
/// # Errors
///
/// Returns a backend error on DB failure, or an internal error when no
/// `[auth] secret` is configured (TOTP cannot seal without it).
pub fn totp_challenge(
    infra: &AppInfra,
    auth_secret: &str,
    slug: &str,
    user: &Document,
) -> Result<Option<TotpProvisioning>, ServiceError> {
    let conn = infra.pool.get().map_err(ServiceError::Internal)?;
    let user_id = user.id.to_string();

    let state = query::get_totp_state(&conn, slug, &user_id)?
        .ok_or_else(|| ServiceError::Internal(anyhow::anyhow!("user not found")))?;

    // Confirmed and openable: nothing to provision.
    if state.confirmed
        && let Some(sealed) = state.sealed_secret.as_deref()
        && open_totp_secret(auth_secret, sealed).is_some()
    {
        return Ok(None);
    }

    // Reuse an existing unconfirmed secret when it still opens — re-showing
    // the same URI keeps a half-finished enrollment resumable.
    if !state.confirmed
        && let Some(sealed) = state.sealed_secret.as_deref()
        && let Some(secret) = open_totp_secret(auth_secret, sealed)
    {
        return Ok(Some(provisioning(user, &secret)));
    }

    // No secret, or it no longer opens (rotated auth secret): start over.
    if state.confirmed {
        error!(
            collection = slug,
            "stored TOTP secret no longer opens (rotated [auth] secret?) — restarting enrollment"
        );
    }

    let secret = generate_totp_secret();
    let sealed = seal_totp_secret(auth_secret, &secret).ok_or_else(|| {
        ServiceError::Internal(anyhow::anyhow!(
            "mfa = \"totp\" requires a configured [auth] secret"
        ))
    })?;

    // Guarded install: only one concurrent first-challenge (or rotation
    // restart) wins, so no surface ever shows provisioning for a secret
    // that is no longer stored.
    if query::set_totp_secret(
        &conn,
        slug,
        &user_id,
        &sealed,
        state.sealed_secret.as_deref(),
    )? {
        // Operator-visible audit signal: a hijacked enrollment (leaked
        // password during the trust-on-first-login window) is detectable.
        info!(collection = slug, user = %user_id, "TOTP enrollment provisioned");

        return Ok(Some(provisioning(user, &secret)));
    }

    // Lost the install race — re-read and show the winner's secret.
    let state = query::get_totp_state(&conn, slug, &user_id)?
        .ok_or_else(|| ServiceError::Internal(anyhow::anyhow!("user not found")))?;

    if let Some(sealed) = state.sealed_secret.as_deref()
        && let Some(secret) = open_totp_secret(auth_secret, sealed)
    {
        return Ok(Some(provisioning(user, &secret)));
    }

    Err(ServiceError::Internal(anyhow::anyhow!(
        "TOTP secret install race left no readable secret"
    )))
}

fn provisioning(user: &Document, secret: &str) -> TotpProvisioning {
    let account = user.fields.get_str("email").unwrap_or("user");

    TotpProvisioning {
        uri: provisioning_uri(account, secret),
        secret: secret.to_string(),
    }
}

/// Verify the second factor for an MFA-gated login, dispatching on the
/// collection's MFA mode: TOTP verifies against the sealed shared secret
/// (advancing the replay guard and confirming enrollment on success);
/// `email` / `custom` verify the stored single-use code.
///
/// This is the ONE chokepoint both login surfaces (admin `/admin/mfa`,
/// gRPC `VerifyMfa`) call.
///
/// # Errors
///
/// Returns a backend error on DB failure.
pub fn verify_second_factor(
    infra: &AppInfra,
    auth_secret: &str,
    slug: &str,
    user_id: &str,
    code: &str,
) -> Result<bool, ServiceError> {
    let mode = infra
        .registry
        .get_collection(slug)
        .and_then(|d| d.auth.as_ref())
        .map_or(MfaMode::Off, Auth::mfa);

    if mode != MfaMode::Totp {
        let conn = infra.pool.get().map_err(ServiceError::Internal)?;
        let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

        return super::verify_mfa_code(&ctx, user_id, code);
    }

    let conn = infra.pool.get().map_err(ServiceError::Internal)?;

    let Some(state) = query::get_totp_state(&conn, slug, user_id)? else {
        return Ok(false);
    };
    let Some(sealed) = state.sealed_secret.as_deref() else {
        return Ok(false);
    };
    let Some(secret) = open_totp_secret(auth_secret, sealed) else {
        error!(
            collection = slug,
            "stored TOTP secret no longer opens (rotated [auth] secret?) — verification impossible \
             until the next challenge restarts enrollment"
        );
        return Ok(false);
    };

    let now = Utc::now().timestamp();

    let Some(step) = verify_totp(&secret, code, now, state.last_step) else {
        return Ok(false);
    };

    // Race-safe: the record is conditional (monotonic step guard) — only
    // the winner of a concurrent double-submit is verified.
    let won = query::record_totp_success(&conn, slug, user_id, step)?;

    if won && !state.confirmed {
        // Audit signal, paired with the provisioning log above.
        info!(collection = slug, user = %user_id, "TOTP enrollment confirmed");
    }

    Ok(won)
}
