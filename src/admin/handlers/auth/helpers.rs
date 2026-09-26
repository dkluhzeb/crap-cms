//! Shared helper functions for auth handlers.

use std::net::SocketAddr;

use axum::{
    http::{HeaderMap, header::COOKIE},
    response::{IntoResponse, Redirect, Response},
};

use crate::{
    admin::{
        AdminState,
        context::{
            AuthBasePageContext, PageMeta, PageType,
            page::auth::{
                AuthCollection, ForgotPasswordPage, LoginPage, MfaPage, ResendVerificationPage,
            },
        },
        handlers::{
            auth::{MFA_PENDING_COOKIE, append_cookies, session_cookies, session_same_site},
            shared::render_auth_page,
        },
        server::{extract_cookie, load_auth_user},
    },
    config::ServerConfig,
    core::{
        ClientIp, CollectionDefinition, Document, Registry,
        auth::Claims,
        collection::{Auth, MfaMode},
        email, spawn_request_blocking,
    },
    service::auth::{
        MintedSession, SessionGrantBuilder, TotpProvisioning, mint_session, totp_challenge,
    },
};

/// The client of an admin request: the TCP peer, or — behind a trusted
/// reverse proxy — the forwarded client, per [`ClientIp::resolve`] (the rule
/// the gRPC API shares). Per-IP limiters key on
/// [`ClientIp::rate_limit_key`]; hooks and logs get the full address.
pub(in crate::admin::handlers) fn client_ip(
    headers: &HeaderMap,
    addr: &SocketAddr,
    server: &ServerConfig,
) -> ClientIp {
    ClientIp::resolve(headers, addr.ip(), server)
}

pub(in crate::admin::handlers) fn login_error(
    state: &AdminState,
    error: &str,
    email: &str,
) -> Response {
    let auth_collections = get_auth_collections(state);
    let show_collection_picker = auth_collections.len() > 1;

    let ctx = LoginPage {
        base: AuthBasePageContext::for_state(
            state,
            PageMeta::new(PageType::AuthLogin, "login_page_title"),
        ),
        error: Some(error.to_string()),
        email: Some(email.to_string()),
        collections: auth_collections,
        show_collection_picker,
        disable_local: all_disable_local(state),
        show_forgot_password: show_forgot_password(state),
        show_resend_verification: show_resend_verification(state),
        success: None,
    };

    render_auth_page(state, "auth/login", &ctx)
}

/// Check if every auth collection has password-login turned off
/// (used by the login page to decide whether to render the
/// email/password inputs at all).
pub(in crate::admin::handlers) fn all_disable_local(state: &AdminState) -> bool {
    let auth_collections: Vec<_> = state
        .infra
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .collect();

    if auth_collections.is_empty() {
        return false;
    }

    auth_collections
        .iter()
        .all(|def| !def.auth.as_ref().is_some_and(Auth::password_login_enabled))
}

/// Check if "forgot password?" link should show on login page.
pub(in crate::admin::handlers) fn show_forgot_password(state: &AdminState) -> bool {
    if !email::is_configured(&state.config.email) {
        return false;
    }

    state
        .infra
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .any(|def| def.auth.as_ref().is_some_and(Auth::forgot_password_enabled))
}

/// Check whether the "didn't get a verification email?" link should show on
/// the login page. Both halves must hold: some collection actually requires
/// verification, and there is a transport to send the link over.
pub(in crate::admin::handlers) fn show_resend_verification(state: &AdminState) -> bool {
    if !email::is_configured(&state.config.email) {
        return false;
    }

    !get_verifying_collections(state).is_empty()
}

pub(in crate::admin::handlers) fn get_auth_collections(state: &AdminState) -> Vec<AuthCollection> {
    auth_collections_where(state, |_| true)
}

/// The auth collections that require email verification — the only ones a
/// resend link means anything for.
pub(in crate::admin::handlers) fn get_verifying_collections(
    state: &AdminState,
) -> Vec<AuthCollection> {
    auth_collections_where(state, |def| {
        def.auth.as_ref().is_some_and(Auth::requires_verify_email)
    })
}

fn auth_collections_where(
    state: &AdminState,
    keep: impl Fn(&CollectionDefinition) -> bool,
) -> Vec<AuthCollection> {
    let mut collections: Vec<AuthCollection> = state
        .infra
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection() && keep(def))
        .map(|def| AuthCollection {
            slug: def.slug.to_string(),
            display_name: def.display_name().to_string(),
        })
        .collect();

    collections.sort_by(|a, b| a.slug.cmp(&b.slug));

    collections
}

pub(in crate::admin::handlers) fn render_forgot_success(
    state: &AdminState,
    auth_collections: &[AuthCollection],
) -> Response {
    let show_collection_picker = auth_collections.len() > 1;

    let ctx = ForgotPasswordPage {
        base: AuthBasePageContext::for_state(
            state,
            PageMeta::new(PageType::AuthForgot, "forgot_password_page_title"),
        ),
        success: true,
        collections: auth_collections.to_vec(),
        show_collection_picker,
    };

    render_auth_page(state, "auth/forgot_password", &ctx)
}

/// Render the resend-verification page, in either the form or the success
/// state. Both states come from one place so the page can never disagree
/// with itself about which collections are on offer.
pub(in crate::admin::handlers) fn render_resend_verification(
    state: &AdminState,
    collections: &[AuthCollection],
    success: bool,
) -> Response {
    let ctx = ResendVerificationPage {
        base: AuthBasePageContext::for_state(
            state,
            PageMeta::new(
                PageType::AuthResendVerification,
                "resend_verification_page_title",
            ),
        ),
        success,
        collections: collections.to_vec(),
        show_collection_picker: collections.len() > 1,
    };

    render_auth_page(state, "auth/resend_verification", &ctx)
}

/// The single auth collection's slug, or `None` when there are zero or 2+.
///
/// The legacy un-scoped OAuth callback (`/admin/auth/callback/{name}`) can only
/// safely bind a session when the target collection is unambiguous. With 2+ auth
/// collections the callback cannot know which one to bind, so it fails closed and
/// the operator must use the collection-scoped route
/// (`/admin/auth/callback/{collection}/{name}`) instead. Either way the
/// hook-returned user must exist in the bound collection — the callback never
/// binds to a different auth collection by id.
pub(in crate::admin::handlers) fn sole_auth_collection(registry: &Registry) -> Option<String> {
    let mut auth = registry
        .collections
        .iter()
        .filter(|(_, d)| d.is_auth_collection())
        .map(|(slug, _)| slug.to_string());

    let only = auth.next()?;

    // 2+ auth collections → ambiguous; force the collection-scoped route.
    if auth.next().is_some() {
        return None;
    }

    Some(only)
}

/// Extract the email field from a user document, defaulting to empty string.
pub(in crate::admin::handlers) fn extract_user_email(user: &Document) -> String {
    user.fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Mint an admin-issued session through the service chokepoint
/// ([`mint_session`]). The caller describes the grant (user, collection,
/// minting surface, second-factor stamp, original `auth_time` on a refresh);
/// this seals it with the admin surface's absolute session ceiling
/// (`[auth] session_absolute_max_age`), so no admin mint site can forget it.
pub(in crate::admin::handlers) fn create_session_token(
    state: &AdminState,
    grant: SessionGrantBuilder<'_>,
) -> Result<MintedSession, String> {
    let grant = grant
        .absolute_max_age(state.config.auth.session_absolute_max_age)
        .build();

    mint_session(&state.infra, &grant).map_err(|e| format!("Session mint error: {e}"))
}

/// Build a redirect-to-/admin response with session cookies set.
pub(in crate::admin::handlers) fn session_redirect(
    state: &AdminState,
    session: &MintedSession,
) -> Response {
    let dev_mode = state.config.admin.dev_mode;
    let same_site = session_same_site(state);
    let cookies = session_cookies(
        &session.token,
        session.lifetime,
        session.exp,
        dev_mode,
        same_site,
    );
    let mut response = Redirect::to("/admin").into_response();

    append_cookies(&mut response, &cookies);

    response
}

/// Extract the `crap_mfa_pending` cookie value from request headers. Shared by
/// the MFA page (GET) and the verify action (POST).
pub(in crate::admin::handlers) fn extract_mfa_token(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(COOKIE)?.to_str().ok()?;

    extract_cookie(cookie_header, MFA_PENDING_COOKIE).map(std::string::ToString::to_string)
}

/// Render the MFA code entry form with an optional error message. Shared by the
/// MFA page (GET) and the verify action (POST, on re-render).
pub(in crate::admin::handlers) fn render_mfa_form(
    state: &AdminState,
    error: Option<&str>,
    totp: bool,
    provisioning: Option<&TotpProvisioning>,
) -> Response {
    let ctx = MfaPage {
        base: AuthBasePageContext::for_state(
            state,
            PageMeta::new(PageType::AuthMfa, "mfa_page_title"),
        ),
        error: error.map(str::to_string),
        totp,
        totp_provisioning_uri: provisioning.map(|p| p.uri.clone()),
        totp_secret: provisioning.map(|p| p.secret.clone()),
    };

    render_auth_page(state, "auth/mfa", &ctx)
}

/// Whether `slug`'s password login uses `mfa = "totp"`.
fn is_totp_collection(state: &AdminState, slug: &str) -> bool {
    state
        .infra
        .registry
        .get_collection(slug)
        .and_then(|d| d.auth.as_ref())
        .is_some_and(|a| a.mfa() == MfaMode::Totp)
}

/// Blocking body: resolve the TOTP page state for the pending user — the
/// mode flag plus provisioning material while enrollment is unconfirmed.
fn totp_state_blocking(state: &AdminState, claims: &Claims) -> (bool, Option<TotpProvisioning>) {
    let slug: &str = claims.collection.as_ref();

    if !is_totp_collection(state, slug) {
        return (false, None);
    }

    let Some(auth_user) = load_auth_user(
        &state.infra.pool,
        &state.infra.registry,
        claims,
        &state.config.locale,
    ) else {
        return (true, None);
    };

    let secret: &str = state.config.auth.secret.as_ref();

    match totp_challenge(&state.infra, secret, slug, &auth_user.user_doc) {
        Ok(p) => (true, p),
        Err(e) => {
            tracing::error!("TOTP challenge: {e:?}");
            (true, None)
        }
    }
}

/// Render the MFA form with the TOTP state resolved — async because the
/// enrollment lookup (and first-challenge secret generation) hits the DB.
pub(in crate::admin::handlers) async fn render_mfa(
    state: &AdminState,
    claims: &Claims,
    error: Option<&str>,
) -> Response {
    let s = state.clone();
    let c = claims.clone();

    let (totp, provisioning) = spawn_request_blocking(move || totp_state_blocking(&s, &c))
        .await
        .unwrap_or((false, None));

    render_mfa_form(state, error, totp, provisioning.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mfa_token_present() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            "crap_csrf=abc; crap_mfa_pending=tok123; other=val"
                .parse()
                .unwrap(),
        );
        assert_eq!(extract_mfa_token(&headers), Some("tok123".to_string()));
    }

    #[test]
    fn extract_mfa_token_missing() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, "crap_csrf=abc; other=val".parse().unwrap());
        assert_eq!(extract_mfa_token(&headers), None);
    }

    #[test]
    fn extract_mfa_token_no_cookie_header() {
        let headers = HeaderMap::new();
        assert_eq!(extract_mfa_token(&headers), None);
    }

    /// The admin wrapper resolves through the shared rule: behind a trusted
    /// appending proxy the rightmost forwarded entry is the client, not the
    /// leftmost one the client wrote itself.
    #[test]
    fn client_ip_uses_the_shared_resolution_rule() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.77, 203.0.113.5".parse().unwrap(),
        );
        let addr: SocketAddr = "10.0.0.5:1234".parse().unwrap();
        let server = ServerConfig {
            trust_proxy: true,
            trusted_proxies: vec!["10.0.0.0/8".to_string()],
            ..ServerConfig::default()
        };

        assert_eq!(
            client_ip(&headers, &addr, &server).to_string(),
            "203.0.113.5"
        );
        assert_eq!(
            client_ip(&headers, &addr, &ServerConfig::default()).to_string(),
            "10.0.0.5"
        );
    }
}
