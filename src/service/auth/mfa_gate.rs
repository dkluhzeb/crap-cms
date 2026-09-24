//! The second-factor gate every verified authentication passes before a
//! session is minted: the password login, a custom-strategy login, and an
//! external auth callback (OAuth / OIDC). One gate, so a new way in cannot
//! quietly skip the collection's MFA requirement.

use std::collections::HashMap;

use tracing::error;

use crate::{
    core::{
        Builder, CollectionDefinition, Document,
        collection::{Auth, MfaMode, Surface},
    },
    db::DbConnection,
    hooks::lifecycle::MfaWhenInput,
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

/// The collection's auth config when this authentication must be judged by
/// the MFA step at all: `None` when the collection has no MFA mode, or the
/// authenticating callback is one it exempts (its identity provider enforces
/// a second factor).
fn gated_auth<'a>(req: &MfaGateRequest<'a>) -> Option<&'a Auth> {
    let auth = req.def.auth.as_ref().filter(|a| a.mfa() != MfaMode::Off)?;

    if req
        .callback
        .is_some_and(|name| auth.mfa_exempts_callback(name))
    {
        return None;
    }

    Some(auth)
}

/// Whether the collection's `mfa_when` hook requires the second factor for
/// this user. No hook = always required; a hook error fails CLOSED — an auth
/// gate that breaks must require more proof, not less.
fn mfa_when_requires(
    infra: &AppInfra,
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

    infra
        .hook_runner
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

/// Route a verified authentication through the collection's MFA requirement:
/// [`LoginOutcome::MfaRequired`] when the surface must run its MFA step
/// before minting a session, [`LoginOutcome::Verified`] otherwise.
pub fn mfa_gate(
    infra: &AppInfra,
    conn: &dyn DbConnection,
    req: &MfaGateRequest<'_>,
    verified: LoginVerified,
) -> LoginOutcome {
    let Some(auth) = gated_auth(req) else {
        return LoginOutcome::Verified(verified);
    };

    if !mfa_when_requires(infra, conn, req, auth, &verified.user) {
        return LoginOutcome::Verified(verified);
    }

    LoginOutcome::MfaRequired(verified)
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
}
