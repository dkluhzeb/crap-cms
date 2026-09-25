//! The second-factor gate every verified authentication passes before a
//! session is minted: the password login, a custom-strategy login, and an
//! external auth callback (OAuth / OIDC). One gate, so a new way in cannot
//! quietly skip the collection's MFA requirement.
//!
//! The same gate judges each request a session token authenticates
//! ([`second_factor_required`]): a session that did not satisfy the second
//! factor is refused wherever the gate — for that request's surface and
//! headers — requires it, so a token minted on a surface without MFA cannot
//! be replayed on one with it.

use std::collections::HashMap;

use tracing::error;

use crate::{
    core::{
        Builder, CollectionDefinition, Document,
        collection::{Auth, MfaMode, Surface},
    },
    db::DbConnection,
    hooks::{HookRunner, lifecycle::MfaWhenInput},
    service::{
        AppInfra,
        auth::{LoginOutcome, LoginVerified},
    },
};

/// One verified authentication, as the MFA gate judges it.
#[derive(Builder)]
pub struct MfaGateRequest<'a> {
    /// The auth collection the session binds to.
    #[builder(required)]
    slug: &'a str,
    /// Its definition (the `password_login` method carries the MFA config).
    #[builder(required)]
    def: &'a CollectionDefinition,
    /// The surface the session is for, exposed to the `mfa_when` gate.
    #[builder(required)]
    surface: Surface,
    /// The request headers, exposed to the `mfa_when` gate.
    #[builder(required)]
    headers: &'a HashMap<String, String>,
    /// The auth callback that authenticated the user — `None` for a login.
    callback: Option<&'a str>,
}

/// The collection's auth config when it has an MFA mode.
fn mfa_auth<'a>(req: &MfaGateRequest<'a>) -> Option<&'a Auth> {
    req.def.auth.as_ref().filter(|a| a.mfa() != MfaMode::Off)
}

/// Whether the authenticating callback is one the collection exempts from
/// its MFA step — its identity provider enforces a second factor, so the
/// session counts as having satisfied one.
fn exempt_callback(req: &MfaGateRequest<'_>) -> bool {
    let Some(auth) = mfa_auth(req) else {
        return false;
    };

    req.callback
        .is_some_and(|name| auth.mfa_exempts_callback(name))
}

/// The collection's auth config when this authentication must be judged by
/// the MFA step at all: `None` when the collection has no MFA mode, or the
/// authenticating callback is one it exempts.
fn gated_auth<'a>(req: &MfaGateRequest<'a>) -> Option<&'a Auth> {
    if exempt_callback(req) {
        return None;
    }

    mfa_auth(req)
}

/// Whether the collection's `mfa_when` hook requires the second factor for
/// this user. No hook = always required; a hook error fails CLOSED — an auth
/// gate that breaks must require more proof, not less.
fn mfa_when_requires(
    hook_runner: &HookRunner,
    conn: &dyn DbConnection,
    req: &MfaGateRequest<'_>,
    auth: &Auth,
    user: &Document,
) -> bool {
    let Some(hook) = auth.mfa_when() else {
        return true;
    };

    let input = MfaWhenInput {
        collection: req.slug,
        user,
        surface: req.surface.as_str(),
        headers: req.headers,
    };

    hook_runner
        .run_mfa_when(hook, &input, conn)
        .inspect_err(|e| {
            error!(
                collection = req.slug,
                hook = hook.reference(),
                error = ?e,
                "mfa_when hook failed; failing closed (requiring MFA)"
            );
        })
        .unwrap_or(true)
}

/// Whether the gate requires the second factor for `user` on this request.
///
/// A login or callback that gets `true` must run the MFA step before any
/// session is minted ([`mfa_gate`]). A session token that did **not** satisfy
/// the second factor and gets `true` for the request it authenticates is
/// refused — the gate is judged for that request's surface and headers
/// (`mfa_when` runs with them), and such a request carries no callback: the
/// session is being used, not established.
#[must_use]
pub fn second_factor_required(
    hook_runner: &HookRunner,
    conn: &dyn DbConnection,
    req: &MfaGateRequest<'_>,
    user: &Document,
) -> bool {
    gated_auth(req).is_some_and(|auth| mfa_when_requires(hook_runner, conn, req, auth, user))
}

/// Route a verified authentication through the collection's MFA requirement:
/// [`LoginOutcome::MfaRequired`] when the surface must run its MFA step
/// before minting a session, [`LoginOutcome::Verified`] otherwise. An
/// exempt callback's authentication comes back marked as having satisfied
/// the second factor, so the session minted from it carries that stamp.
pub fn mfa_gate(
    infra: &AppInfra,
    conn: &dyn DbConnection,
    req: &MfaGateRequest<'_>,
    mut verified: LoginVerified,
) -> LoginOutcome {
    if exempt_callback(req) {
        verified.mfa = true;

        return LoginOutcome::Verified(verified);
    }

    if second_factor_required(&infra.hook_runner, conn, req, &verified.user) {
        return LoginOutcome::MfaRequired(verified);
    }

    LoginOutcome::Verified(verified)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(auth: Auth) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(auth);

        def
    }

    fn totp_exempting(name: &str) -> Auth {
        Auth::enabled().map_password_login(|b| {
            b.mfa(MfaMode::Totp)
                .mfa_exempt_callbacks(vec![name.to_string()])
        })
    }

    fn gated(def: &CollectionDefinition, callback: Option<&str>) -> bool {
        let headers = HashMap::new();
        let req = MfaGateRequest::builder("users", def, Surface::Admin, &headers)
            .callback(callback)
            .build();

        gated_auth(&req).is_some()
    }

    /// A collection without an MFA mode never gates.
    #[test]
    fn no_mfa_mode_never_gates() {
        let def = def(Auth::enabled());

        assert!(!gated(&def, None));
        assert!(!gated(&def, Some("okta")));
    }

    /// A login and every callback the collection does not exempt are gated;
    /// only an exempt callback skips the MFA step.
    #[test]
    fn only_an_exempt_callback_skips_the_gate() {
        let def = def(totp_exempting("okta"));

        assert!(gated(&def, None), "a login is gated");
        assert!(
            gated(&def, Some("google")),
            "a non-exempt callback is gated"
        );
        assert!(!gated(&def, Some("okta")), "the exempt callback is not");
    }

    fn exempt(def: &CollectionDefinition, callback: Option<&str>) -> bool {
        let headers = HashMap::new();
        let req = MfaGateRequest::builder("users", def, Surface::Admin, &headers)
            .callback(callback)
            .build();

        exempt_callback(&req)
    }

    /// Only a callback the collection names — on a collection with an MFA
    /// mode — counts as having satisfied the second factor; its session is
    /// stamped so. A login never is.
    #[test]
    fn only_a_named_callback_satisfies_the_second_factor() {
        let with_mfa = def(totp_exempting("okta"));
        let without_mfa = def(Auth::enabled());

        assert!(exempt(&with_mfa, Some("okta")));
        assert!(!exempt(&with_mfa, Some("google")));
        assert!(!exempt(&with_mfa, None));
        assert!(!exempt(&without_mfa, Some("okta")));
    }
}
