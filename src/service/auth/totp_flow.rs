//! TOTP challenge + verification flow — the service-level counterpart of
//! [`super::mfa`] for `mfa = "totp"`.
//!
//! Enrollment is challenge-driven: the first MFA challenge generates and
//! seals a secret; the provisioning URI is returned (and re-returned) until
//! the first successful verification confirms enrollment. Verification is
//! replay-guarded via the persisted last accepted time step.

use anyhow::anyhow;
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
    db::{
        DbConnection,
        query::{self, MfaCode, TotpState},
    },
    service::{AppInfra, ServiceContext, ServiceError, admit_commit, auth::verify_mfa_code},
};

/// The enrollment material shown while a user's TOTP is unconfirmed.
pub struct TotpProvisioning {
    /// `otpauth://` URI (QR payload / tap-to-add link).
    pub uri: String,
    /// The base32 secret, for manual entry.
    pub secret: String,
}

/// The user's stored TOTP state; a missing user is an internal error (the
/// challenge only runs for a user that just authenticated).
fn stored_state(
    conn: &dyn DbConnection,
    slug: &str,
    user_id: &str,
) -> Result<TotpState, ServiceError> {
    query::get_totp_state(conn, slug, user_id)?
        .ok_or_else(|| ServiceError::Internal(anyhow!("user not found")))
}

/// The user a TOTP challenge is for. Plain literal, built once.
struct Enrollment<'a> {
    auth_secret: &'a str,
    slug: &'a str,
    user: &'a Document,
}

/// What the stored enrollment answers for the next challenge.
enum StoredEnrollment {
    /// The stored secret opens: nothing to provision when it is confirmed,
    /// or the provisioning to re-show for an unconfirmed one (a
    /// half-finished enrollment stays resumable).
    Answered(Option<TotpProvisioning>),
    /// No secret, or one that no longer opens: a fresh one must be installed.
    NeedsInstall,
}

/// Judge the stored enrollment (see [`StoredEnrollment`]).
fn current_enrollment(e: &Enrollment<'_>, state: &TotpState) -> StoredEnrollment {
    let opened = state
        .sealed_secret
        .as_deref()
        .and_then(|sealed| open_totp_secret(e.auth_secret, sealed));

    match (state.confirmed, opened) {
        (true, Some(_)) => StoredEnrollment::Answered(None),
        (false, Some(secret)) => StoredEnrollment::Answered(Some(provisioning(e.user, &secret))),
        (true, None) => {
            error!(
                collection = e.slug,
                "stored TOTP secret no longer opens (rotated [auth] secret?) — restarting enrollment"
            );

            StoredEnrollment::NeedsInstall
        }
        (false, None) => StoredEnrollment::NeedsInstall,
    }
}

/// Generate, seal and install a fresh secret for `user`, replacing the one
/// `state` holds (none, or one that no longer opens). The install is guarded:
/// only one concurrent first challenge (or rotation restart) wins, and a
/// loser shows the winner's secret — no surface ever shows provisioning for a
/// secret that is not stored.
fn install_secret(
    infra: &AppInfra,
    e: &Enrollment<'_>,
    state: &TotpState,
) -> Result<Option<TotpProvisioning>, ServiceError> {
    let Enrollment {
        auth_secret,
        slug,
        user,
    } = *e;
    let user_id = user.id.to_string();

    let secret = generate_totp_secret();
    let sealed = seal_totp_secret(auth_secret, &secret).ok_or_else(|| {
        ServiceError::Internal(anyhow!(
            "mfa = \"totp\" requires a configured [auth] secret"
        ))
    })?;

    let conn = infra
        .pool
        .write()
        .map_err(|e| ServiceError::classify(e, infra.pool.kind()))?;

    let replaced = state.sealed_secret.as_deref();

    // The install is a single autocommit write: its own commit point.
    admit_commit()?;

    if query::set_totp_secret(&conn, slug, &user_id, &sealed, replaced)? {
        // Operator-visible audit signal: a hijacked enrollment (leaked
        // password during the trust-on-first-login window) is detectable.
        info!(collection = slug, user = %user_id, "TOTP enrollment provisioned");

        return Ok(Some(provisioning(user, &secret)));
    }

    // Lost the install race — re-read and show the winner's secret.
    let state = stored_state(&conn, slug, &user_id)?;

    state
        .sealed_secret
        .as_deref()
        .and_then(|sealed| open_totp_secret(auth_secret, sealed))
        .map(|secret| Some(provisioning(user, &secret)))
        .ok_or_else(|| {
            ServiceError::Internal(anyhow!("TOTP secret install race left no readable secret"))
        })
}

/// Prepare the TOTP side of an MFA challenge for `user`. Returns the
/// provisioning material while enrollment is unconfirmed, `None` once
/// confirmed (the user just enters their authenticator code).
///
/// Generates + seals a fresh secret when none exists — and also when the
/// stored one no longer opens (a rotated `[auth] secret`): an unconfirmed
/// or unopenable enrollment restarts rather than stranding the login. The
/// state is read on a read connection; only an install takes a write one.
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
    let conn = infra
        .pool
        .get()
        .map_err(|e| ServiceError::classify(e, infra.pool.kind()))?;

    let state = stored_state(&conn, slug, &user.id)?;

    drop(conn);

    let enrollment = Enrollment {
        auth_secret,
        slug,
        user,
    };

    if let StoredEnrollment::Answered(outcome) = current_enrollment(&enrollment, &state) {
        return Ok(outcome);
    }

    install_secret(infra, &enrollment, &state)
}

fn provisioning(user: &Document, secret: &str) -> TotpProvisioning {
    let account = user.fields.get_str("email").unwrap_or("user");

    TotpProvisioning {
        uri: provisioning_uri(account, secret),
        secret: secret.to_string(),
    }
}

/// Verify `attempt` against the user's TOTP enrollment: the sealed shared
/// secret, advancing the replay guard (and confirming enrollment) on success.
fn verify_totp_attempt(
    conn: &dyn DbConnection,
    slug: &str,
    attempt: &MfaCode,
) -> Result<bool, ServiceError> {
    let MfaCode {
        user_id,
        code,
        auth_secret,
    } = *attempt;

    let Some(state) = query::get_totp_state(conn, slug, user_id)? else {
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

    let Some(step) = verify_totp(&secret, code, Utc::now().timestamp(), state.last_step) else {
        return Ok(false);
    };

    // Race-safe: the record is conditional (monotonic step guard) — only
    // the winner of a concurrent double-submit is verified. A single
    // autocommit write: its own commit point.
    admit_commit()?;

    let won = query::record_totp_success(conn, slug, user_id, step)?;

    if won && !state.confirmed {
        // Audit signal, paired with the provisioning log above.
        info!(collection = slug, user = %user_id, "TOTP enrollment confirmed");
    }

    Ok(won)
}

/// Verify the second factor `attempt` for an MFA-gated login to the auth
/// collection `slug`, dispatching on the collection's MFA mode: TOTP verifies
/// against the sealed shared secret (advancing the replay guard and
/// confirming enrollment on success); `email` / `custom` verify the stored
/// single-use code.
///
/// This is the ONE chokepoint both login surfaces (admin `/admin/mfa`,
/// gRPC `VerifyMfa`) call.
///
/// # Errors
///
/// Returns a backend error on DB failure.
pub fn verify_second_factor(
    infra: &AppInfra,
    slug: &str,
    attempt: &MfaCode,
) -> Result<bool, ServiceError> {
    let mode = infra
        .registry
        .get_collection(slug)
        .and_then(|d| d.auth.as_ref())
        .map_or(MfaMode::Off, Auth::mfa);

    // Both verifications write: the stored code is consumed, the TOTP replay
    // guard advanced.
    let conn = infra
        .pool
        .write()
        .map_err(|e| ServiceError::classify(e, infra.pool.kind()))?;

    if mode == MfaMode::Totp {
        return verify_totp_attempt(&conn, slug, attempt);
    }

    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

    // Consuming the code is a single autocommit write: its own commit point.
    admit_commit()?;

    verify_mfa_code(&ctx, attempt)
}
