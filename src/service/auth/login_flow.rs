//! The shared credential-verification flow behind login.
//!
//! The gRPC `Login` RPC and the admin login action used to each carry a
//! near-copy of this sequence (local password auth → custom strategy
//! fallback → locked/verified/session-version checks → timing
//! equalization), and the copies had drifted: the admin twin drew its
//! connection from the READ pool while `authenticate_local` writes lockout
//! counters, the gRPC twin silently swallowed strategy errors the admin twin
//! logged, and only the admin twin enforced MFA. One flow, one behavior.
//!
//! Surfaces stay codecs: rate limiting, wire decode, and the success shape
//! (JWT response vs session cookie vs MFA challenge) remain per surface.

use std::collections::HashMap;

use tracing::error;

use crate::{
    core::{
        CollectionDefinition, Document,
        auth::PasswordProvider,
        collection::{Activation, Auth, MfaMode, StrategyCfg, Surface},
    },
    db::BoxedConnection,
    hooks::{
        HookRunner,
        lifecycle::{AuthStrategyInput, MfaWhenInput},
    },
    service::{AppInfra, ServiceContext, ServiceError, auth::authenticate_local},
};

/// Verified credentials: the user document and its current session version.
pub struct LoginVerified {
    pub user: Document,
    pub session_version: u64,
}

/// Outcome of [`verify_login`].
pub enum LoginOutcome {
    /// Credentials verified and no MFA required — the surface may mint a
    /// session.
    Verified(LoginVerified),
    /// Credentials verified but the collection requires email MFA. The
    /// surface must run its MFA step; minting a full session here would
    /// bypass the second factor.
    MfaRequired(LoginVerified),
    /// Recoverable failure (unknown user, wrong password, locked,
    /// unverified) — deny uniformly, leaking nothing about which.
    Denied,
}

/// Per-call inputs for [`verify_login`]. All fields required; constructed at
/// the two login codecs — plain struct literal.
pub struct LoginFlowRequest<'a> {
    pub slug: &'a str,
    pub def: &'a CollectionDefinition,
    pub email: &'a str,
    pub password: &'a str,
    /// Transport headers, exposed to custom auth strategies and the
    /// `mfa_when` gate.
    pub headers: &'a HashMap<String, String>,
    /// Client address, exposed to custom auth strategies.
    pub remote_addr: Option<&'a str>,
    /// The login surface, exposed to the `mfa_when` gate so MFA can apply
    /// per surface.
    pub surface: Surface,
    pub password_provider: &'a dyn PasswordProvider,
}

/// Verify login credentials: local email+password first, then any configured
/// custom strategies; strategy-authenticated users get the same
/// locked/verified/session-version checks (fail closed). Draws a WRITE
/// connection — local auth updates lockout counters on it.
///
/// Returns [`LoginOutcome::Denied`] for every recoverable failure; timing is
/// equalized with a dummy password verification so "no such user" and
/// "wrong password" are indistinguishable.
///
/// # Errors
///
/// Returns an error only for system failures (pool, DB, hook runtime).
pub fn verify_login(
    infra: &AppInfra,
    req: &LoginFlowRequest<'_>,
) -> Result<LoginOutcome, ServiceError> {
    let conn = infra
        .pool
        .write()
        .map_err(ServiceError::Internal)
        .inspect_err(|e| error!("Login DB connection error: {e}"))?;

    let allows_password = req
        .def
        .auth
        .as_ref()
        .is_some_and(Auth::password_login_enabled);
    let require_verified = req
        .def
        .auth
        .as_ref()
        .is_some_and(Auth::requires_verify_email);

    // Local email+password authentication via the service chokepoint.
    if allows_password {
        let ctx = ServiceContext::collection(req.slug, req.def)
            .conn(&conn)
            .build();

        match authenticate_local(
            &ctx,
            req.email,
            req.password,
            req.password_provider,
            require_verified,
        ) {
            Ok(result) => {
                return Ok(mfa_gate(
                    infra,
                    &conn,
                    req,
                    LoginVerified {
                        user: result.user,
                        session_version: result.session_version,
                    },
                ));
            }
            // Recoverable — fall through to strategies (whose results are
            // re-checked for locked/verified below).
            Err(
                ServiceError::InvalidCredentials
                | ServiceError::AccountLocked
                | ServiceError::EmailNotVerified,
            ) => {}
            Err(e) => return Err(e),
        }
    }

    // Fallback: custom auth strategies (Lua). Credentials and the client
    // address are exposed so a strategy can verify against an external system.
    if let Some(user) = try_strategy_auth(&conn, req, &infra.hook_runner) {
        let ctx = ServiceContext::slug_only(req.slug).conn(&conn).build();

        // Strategy-authenticated users still need locked/verified checks. A
        // lookup failure must fail CLOSED (deny) — letting a locked account
        // in on a transient DB error is an auth bypass.
        if crate::service::auth::is_locked(&ctx, &user.id)? {
            return Ok(LoginOutcome::Denied);
        }

        if require_verified && !crate::service::auth::is_verified(&ctx, &user.id)? {
            return Ok(LoginOutcome::Denied);
        }

        let session_version = crate::service::auth::get_session_version(&ctx, &user.id)?;

        return Ok(mfa_gate(
            infra,
            &conn,
            req,
            LoginVerified {
                user,
                session_version,
            },
        ));
    }

    // Equalize timing when all auth methods fail — prevents distinguishing
    // "no valid user" (fast) from "wrong password" (Argon2-slow) via
    // response time.
    if allows_password {
        req.password_provider.dummy_verify();
    }

    Ok(LoginOutcome::Denied)
}

/// Route a verified login through the collection's MFA requirement.
///
/// When an `mfa_when` gate hook is configured it decides whether THIS login
/// needs the second factor (per surface / per user field); a hook error
/// fails CLOSED — an auth gate that breaks must require more proof, not
/// less.
fn mfa_gate(
    infra: &AppInfra,
    conn: &BoxedConnection,
    req: &LoginFlowRequest<'_>,
    verified: LoginVerified,
) -> LoginOutcome {
    let mfa_enabled = req
        .def
        .auth
        .as_ref()
        .is_some_and(|a| a.mfa() != MfaMode::Off);

    if !mfa_enabled {
        return LoginOutcome::Verified(verified);
    }

    if let Some(hook) = req.def.auth.as_ref().and_then(Auth::mfa_when) {
        let input = MfaWhenInput {
            collection: req.slug,
            user: &verified.user,
            surface: req.surface.as_str(),
            headers: req.headers,
        };

        match infra.hook_runner.run_mfa_when(hook, &input, conn) {
            Ok(false) => return LoginOutcome::Verified(verified),
            Ok(true) => {}
            Err(e) => {
                error!(
                    collection = req.slug,
                    hook = hook.reference(),
                    error = ?e,
                    "mfa_when hook failed; failing closed (requiring MFA)"
                );
            }
        }
    }

    LoginOutcome::MfaRequired(verified)
}

/// Try each configured auth strategy in order, returning the first match.
/// Strategy errors are logged and skipped — operators need visibility into
/// failures (DB errors, bad config, Lua panics) that would otherwise silence
/// themselves as "authentication failed".
fn try_strategy_auth(
    conn: &BoxedConnection,
    req: &LoginFlowRequest<'_>,
    hook_runner: &HookRunner,
) -> Option<Document> {
    let auth = req.def.auth.as_ref()?;

    let strategy_input = AuthStrategyInput {
        collection: req.slug,
        headers: req.headers,
        email: Some(req.email),
        password: Some(req.password),
        remote_addr: req.remote_addr,
    };

    for strategy in auth
        .strategies()
        .filter(|s| strategy_applies(s, req.surface, req.headers))
    {
        match hook_runner.run_auth_strategy(strategy.authenticate, &strategy_input, conn) {
            Ok(Some(doc)) => return Some(doc),
            Ok(None) => {}
            Err(e) => {
                error!(
                    collection = req.slug,
                    strategy = strategy.authenticate.reference(),
                    error = ?e,
                    "Custom auth strategy returned an error; continuing to next strategy"
                );
            }
        }
    }

    None
}

/// Does `strategy` apply to this login attempt? Mirrors the request-time
/// evaluator's scoping: the strategy must list the login's surface, and its
/// `activates_on` discriminator must match the request (`always`, or the
/// named header present — compared case-insensitively). A strategy declared
/// `surfaces = {"grpc"}, activates_on = { header = "x-api-key" }` therefore
/// never runs on an admin form POST, exactly as documented.
fn strategy_applies(
    strategy: &StrategyCfg<'_>,
    surface: Surface,
    headers: &HashMap<String, String>,
) -> bool {
    if !strategy.surfaces.contains(surface) {
        return false;
    }

    match strategy.activates_on {
        Activation::Always { .. } => true,
        Activation::Header { header } => headers.keys().any(|k| k.eq_ignore_ascii_case(header)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{HookRef, collection::SurfaceSet};

    fn cfg<'a>(
        activates_on: &'a Activation,
        surfaces: &'a SurfaceSet,
        authenticate: &'a HookRef,
    ) -> StrategyCfg<'a> {
        StrategyCfg {
            name: "t",
            authenticate,
            activates_on,
            surfaces,
        }
    }

    /// Regression: the login path used to run EVERY strategy on the
    /// collection regardless of `surfaces` / `activates_on`.
    #[test]
    fn login_path_strategy_scoping_honors_surface_and_activation() {
        let hook = HookRef::new("hooks.auth.key");
        let header = Activation::header("x-api-key");
        let always = Activation::always();
        let grpc = SurfaceSet::grpc_only();
        let admin = SurfaceSet::admin_only();
        let none: HashMap<String, String> = HashMap::new();
        let with_key: HashMap<String, String> =
            HashMap::from([("X-Api-Key".to_string(), "k".to_string())]);

        // Wrong surface → never, even with the header present.
        assert!(!strategy_applies(
            &cfg(&header, &grpc, &hook),
            Surface::Admin,
            &with_key
        ));
        // Right surface, header absent → no.
        assert!(!strategy_applies(
            &cfg(&header, &grpc, &hook),
            Surface::Grpc,
            &none
        ));
        // Right surface, header present (case-insensitive) → yes.
        assert!(strategy_applies(
            &cfg(&header, &grpc, &hook),
            Surface::Grpc,
            &with_key
        ));
        // Always-active on its surface → yes; off its surface → no.
        assert!(strategy_applies(
            &cfg(&always, &admin, &hook),
            Surface::Admin,
            &none
        ));
        assert!(!strategy_applies(
            &cfg(&always, &admin, &hook),
            Surface::Grpc,
            &none
        ));
    }
}
