//! The shared credential-verification flow behind login.
//!
//! The gRPC `Login` RPC and the admin login action used to each carry a
//! near-copy of this sequence (local password auth → custom strategy
//! fallback → locked/verified/session-version checks → timing
//! equalization), and the copies had drifted: the gRPC twin silently
//! swallowed strategy errors the admin twin logged, and only the admin twin
//! enforced MFA. One flow, one behavior.
//!
//! Surfaces stay codecs: rate limiting, wire decode, and the success shape
//! (JWT response vs session cookie vs MFA challenge) remain per surface.

use std::collections::HashMap;

use anyhow::Result as AnyResult;
use tracing::error;

use crate::{
    config::PasswordPolicy,
    core::{
        Builder, CollectionDefinition, Document,
        auth::PasswordProvider,
        collection::{Activation, Auth, StrategyCfg, Surface},
    },
    db::BoxedConnection,
    hooks::lifecycle::AuthStrategyInput,
    service::{
        AppInfra, ServiceContext, ServiceError,
        auth::{
            MfaGateRequest, StrategyAdmission, admit_strategy_user, authenticate_local, mfa_gate,
        },
    },
};

/// The least a login accepts as a password length, in bytes, whatever
/// `[auth.password_policy] max_length` says. Lowering the policy bounds new
/// passwords only; passwords set under a higher bound keep working up to this
/// floor (or the policy, if higher), while an unbounded input still never
/// reaches the hash.
const LOGIN_PASSWORD_FLOOR_BYTES: usize = 1024;

/// Verified credentials: the user document and its current session version.
#[derive(Builder)]
pub struct LoginVerified {
    #[builder(required)]
    pub user: Document,
    #[builder(required)]
    pub session_version: u64,
    /// Whether the authentication already satisfies the second factor — set
    /// by [`mfa_gate`] for an auth callback the collection exempts (its
    /// identity provider enforces one). The session minted from it carries
    /// the stamp.
    pub mfa: bool,
}

/// Outcome of [`verify_login`].
pub enum LoginOutcome {
    /// Credentials verified and no MFA required — the surface may mint a
    /// session.
    Verified(LoginVerified),
    /// Credentials verified but the collection requires its second factor
    /// (see [`mfa_gate`]). The surface must run its MFA step; minting a full
    /// session here would bypass the second factor.
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
/// locked/verified/session-version checks (fail closed).
///
/// A password longer than the login cap — `[auth.password_policy]
/// max_length`, but never less than [`LOGIN_PASSWORD_FLOOR_BYTES`] — is
/// refused before any lookup, hash or strategy runs: hashing an arbitrarily
/// long one is the expensive part of a login. The floor keeps a lowered
/// `max_length` (which bounds passwords when they are SET) from locking out
/// users whose existing password is longer. The refusal does not depend on
/// the account, so it reveals nothing.
///
/// The password path holds no connection while Argon2 runs and never takes a
/// WRITE connection: its lookups and the `mfa_when` gate are reads. A
/// strategy login runs on a read connection too: only a strategy hook's first
/// write — provisioning the user, say — opens a transaction on a write
/// connection (see
/// [`HookRunner::run_auth_strategy`](crate::hooks::HookRunner::run_auth_strategy)).
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
    if password_exceeds_login_cap(&infra.password_policy, req.password) {
        return Ok(LoginOutcome::Denied);
    }

    let auth = req.def.auth.as_ref();
    let allows_password = auth.is_some_and(Auth::password_login_enabled);
    let require_verified = auth.is_some_and(Auth::requires_verify_email);

    if allows_password && let Some(outcome) = password_login(infra, req, require_verified)? {
        return Ok(outcome);
    }

    // Fallback: custom auth strategies (Lua).
    if let Some(outcome) = strategy_login(infra, req, require_verified)? {
        return Ok(outcome);
    }

    // Equalize timing when all auth methods fail — prevents distinguishing
    // "no valid user" (fast) from "wrong password" (Argon2-slow) via
    // response time.
    if allows_password {
        req.password_provider.dummy_verify();
    }

    Ok(LoginOutcome::Denied)
}

/// Whether `password` is longer (in bytes) than a login accepts: the policy's
/// `max_length`, floored at [`LOGIN_PASSWORD_FLOOR_BYTES`].
fn password_exceeds_login_cap(policy: &PasswordPolicy, password: &str) -> bool {
    password.len() > policy.max_length.max(LOGIN_PASSWORD_FLOOR_BYTES)
}

/// Local email+password authentication via the service chokepoint: `None`
/// when the credentials do not authenticate (unknown user, wrong password,
/// locked, unverified), so the strategies get their turn.
///
/// Runs against the pool, so [`authenticate_local`] takes a read connection
/// per lookup and none across the password hash.
fn password_login(
    infra: &AppInfra,
    req: &LoginFlowRequest<'_>,
    require_verified: bool,
) -> Result<Option<LoginOutcome>, ServiceError> {
    let ctx = ServiceContext::collection(req.slug, req.def)
        .pool(&infra.pool)
        .locale_config(Some(&infra.locale_config))
        .build();

    let result = match authenticate_local(
        &ctx,
        req.email,
        req.password,
        req.password_provider,
        require_verified,
    ) {
        Ok(result) => result,
        Err(
            ServiceError::InvalidCredentials
            | ServiceError::AccountLocked
            | ServiceError::EmailNotVerified,
        ) => return Ok(None),
        Err(e) => return Err(e),
    };

    let conn = connection(infra.pool.get(), "read")?;
    let verified = LoginVerified::builder(result.user, result.session_version).build();

    Ok(Some(mfa_gate(infra, &conn, &gate_request(req), verified)))
}

/// A pooled connection, or the logged internal error a login reports.
fn connection(
    checkout: AnyResult<BoxedConnection>,
    kind: &str,
) -> Result<BoxedConnection, ServiceError> {
    checkout
        .inspect_err(|e| error!("Login DB {kind} connection error: {e}"))
        .map_err(ServiceError::Internal)
}

/// Log in through the collection's custom auth strategies (Lua): `None` when
/// no strategy named a user. Credentials and the client address are exposed
/// so a strategy can verify against an external system.
///
/// The strategy's table only names the user; the login continues with the
/// stored document, admitted exactly as a strategy-authenticated request is
/// ([`admit_strategy_user`]): the stored row decides lock / verification
/// state, the hook's flags may only restrict. A lookup failure propagates
/// (fail CLOSED) — letting a locked account in on a transient DB error is an
/// auth bypass.
fn strategy_login(
    infra: &AppInfra,
    req: &LoginFlowRequest<'_>,
    require_verified: bool,
) -> Result<Option<LoginOutcome>, ServiceError> {
    let Some(auth) = req.def.auth.as_ref() else {
        return Ok(None);
    };

    let applicable: Vec<StrategyCfg<'_>> = auth
        .strategies()
        .filter(|s| strategy_applies(s, req.surface, req.headers))
        .collect();

    if applicable.is_empty() {
        return Ok(None);
    }

    let conn = connection(infra.pool.get(), "read")?;

    let Some(named) = try_strategy_auth(&conn, req, &applicable, infra) else {
        return Ok(None);
    };

    let ctx = ServiceContext::collection(req.slug, req.def)
        .conn(&conn)
        .locale_config(Some(&infra.locale_config))
        .build();

    let StrategyAdmission::Admitted {
        user,
        session_version,
    } = admit_strategy_user(&ctx, &named, require_verified)?
    else {
        return Ok(Some(LoginOutcome::Denied));
    };

    let verified = LoginVerified::builder(user, session_version).build();

    Ok(Some(mfa_gate(infra, &conn, &gate_request(req), verified)))
}

/// The MFA gate request for this login.
fn gate_request<'a>(req: &LoginFlowRequest<'a>) -> MfaGateRequest<'a> {
    MfaGateRequest::builder(req.slug, req.def, req.surface, req.headers).build()
}

/// Try each applicable auth strategy in order, returning the first match.
/// Strategy errors are logged and skipped — operators need visibility into
/// failures (DB errors, bad config, Lua panics) that would otherwise silence
/// themselves as "authentication failed".
fn try_strategy_auth(
    conn: &BoxedConnection,
    req: &LoginFlowRequest<'_>,
    strategies: &[StrategyCfg<'_>],
    infra: &AppInfra,
) -> Option<Document> {
    let strategy_input = AuthStrategyInput {
        collection: req.slug,
        headers: req.headers,
        email: Some(req.email),
        password: Some(req.password),
        remote_addr: req.remote_addr,
    };

    for strategy in strategies {
        match infra.hook_runner.run_auth_strategy(
            strategy.authenticate,
            &strategy_input,
            conn,
            infra,
        ) {
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

    fn policy_of(max_length: usize) -> PasswordPolicy {
        PasswordPolicy {
            max_length,
            ..PasswordPolicy::default()
        }
    }

    /// The cap counts bytes, and a policy below the floor is capped at the
    /// floor.
    #[test]
    fn the_login_cap_is_the_floor_in_bytes_below_it() {
        let policy = policy_of(128);

        assert!(!password_exceeds_login_cap(
            &policy,
            &"a".repeat(LOGIN_PASSWORD_FLOOR_BYTES)
        ));
        assert!(password_exceeds_login_cap(
            &policy,
            &"a".repeat(LOGIN_PASSWORD_FLOOR_BYTES + 1)
        ));
        assert!(password_exceeds_login_cap(
            &policy,
            &"ä".repeat(LOGIN_PASSWORD_FLOOR_BYTES / 2 + 1)
        ));
    }

    /// Regression: the login cap was `max_length` itself, so lowering it
    /// refused every existing password longer than the new bound — those
    /// users were locked out of password login.
    #[test]
    fn lowering_the_policy_does_not_refuse_a_longer_existing_password() {
        assert!(!password_exceeds_login_cap(&policy_of(8), &"a".repeat(64)));
    }

    #[test]
    fn a_policy_above_the_floor_is_the_login_cap() {
        let policy = policy_of(4096);

        assert!(!password_exceeds_login_cap(&policy, &"a".repeat(4096)));
        assert!(password_exceeds_login_cap(&policy, &"a".repeat(4097)));
    }

    #[cfg(feature = "sqlite")]
    mod overlong_password {
        use super::*;
        use crate::{
            admin::test_support::test_infra_with_events,
            core::{FieldDefinition, FieldType, HashedPassword},
        };

        /// A provider that fails the test the moment anything is hashed.
        struct NoHashing;

        impl PasswordProvider for NoHashing {
            fn hash_password(&self, _password: &str) -> AnyResult<HashedPassword> {
                panic!("a refused login must not hash")
            }

            fn verify_password(&self, _password: &str, _hash: &str) -> AnyResult<bool> {
                panic!("a refused login must not verify a hash")
            }

            fn dummy_verify(&self) {
                panic!("a refused login must not run the dummy hash")
            }

            fn kind(&self) -> &'static str {
                "no-hashing"
            }
        }

        /// Regression: login hashed a password of any length (up to the request
        /// body limit), so one request could buy an arbitrarily long Argon2 pass.
        /// A password longer than the login cap is now refused before any
        /// lookup or hash.
        #[test]
        fn an_overlong_password_is_denied_without_hashing() {
            let mut def = CollectionDefinition::new("users");
            def.auth = Some(Auth::enabled());
            def.fields = vec![
                FieldDefinition::builder("email", FieldType::Email)
                    .unique(true)
                    .build(),
            ];

            let (_tmp, infra, _rx) = test_infra_with_events(def.clone());
            let headers = HashMap::new();
            let cap = infra
                .password_policy
                .max_length
                .max(LOGIN_PASSWORD_FLOOR_BYTES);
            let password = "a".repeat(cap + 1);

            let outcome = verify_login(
                &infra,
                &LoginFlowRequest {
                    slug: "users",
                    def: &def,
                    email: "someone@example.com",
                    password: &password,
                    headers: &headers,
                    remote_addr: None,
                    surface: Surface::Admin,
                    password_provider: &NoHashing,
                },
            )
            .expect("a refusal is not a system error");

            assert!(matches!(outcome, LoginOutcome::Denied));
        }
    }

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
