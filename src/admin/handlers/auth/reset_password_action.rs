use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use tracing::error;

use crate::{
    admin::{
        AdminState, Translations,
        context::{AuthBasePageContext, PageMeta, PageType, page::auth::ResetPasswordPage},
        handlers::{
            auth::{ResetPasswordForm, client_ip},
            shared::{paths, render_auth_page},
        },
    },
    config::PasswordViolation,
    core::{rate_limit::IP_RESET_PASSWORD_KEYSPACE, spawn_request_blocking},
    service::{
        AppInfra, ServiceError,
        auth::{PasswordReset, reset_password_with_token},
    },
};

/// The reset page's shared context.
fn reset_page_base(state: &AdminState) -> AuthBasePageContext {
    AuthBasePageContext::for_state(
        state,
        PageMeta::new(PageType::AuthReset, "reset_password_page_title"),
    )
}

/// Render the reset page with `error` (the template translates it) and the
/// token, when the form should stay usable.
fn render_reset_page(
    state: &AdminState,
    base: AuthBasePageContext,
    token: Option<&str>,
    error: String,
) -> Response {
    let ctx = ResetPasswordPage {
        base,
        token: token.map(str::to_string),
        error: Some(error),
    };

    render_auth_page(state, "auth/reset_password", &ctx)
}

/// Render a reset password error page with the given error key and optional token.
fn render_reset_error(state: &AdminState, token: Option<&str>, error: &str) -> Response {
    render_reset_page(state, reset_page_base(state), token, error.to_string())
}

/// A password-policy violation in `locale`: the violation's translation key
/// with its params (the minimum or maximum length) filled in.
fn violation_message(
    translations: &Translations,
    locale: &str,
    violation: PasswordViolation,
) -> String {
    let params: HashMap<String, String> = violation
        .params()
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect();

    translations.get_interpolated(locale, violation.translation_key(), &params)
}

/// Render the reset page for a password-policy violation, in the page's
/// locale — the English `Display` text is for the API surfaces.
fn render_policy_violation(
    state: &AdminState,
    token: &str,
    violation: PasswordViolation,
) -> Response {
    let base = reset_page_base(state);
    let message = violation_message(&state.translations, &base.locale, violation);

    render_reset_page(state, base, Some(token), message)
}

/// Reset the password with the token, searching every auth collection for it.
/// The service owns the transaction (committed under the request's commit gate
/// only on success) and the post-commit live-stream teardown.
fn reset_password_blocking(
    infra: &AppInfra,
    token: &str,
    password: &str,
) -> Result<(), ServiceError> {
    let candidates = infra.registry.collections.values().map(Arc::as_ref);

    reset_password_with_token(infra, candidates, &PasswordReset::new(token, password))
}

/// The error the reset page shows for a failed reset. Matches the TYPED
/// refusal: an expired link reads as expired, any other token refusal as
/// invalid. Anything else is not the link's fault — the backend failed or the
/// request ran out of time — so it reads as an internal error, logged here.
fn reset_error_key(e: &ServiceError) -> &'static str {
    match e {
        ServiceError::InvalidToken {
            reason: "expired", ..
        } => "error_reset_link_expired",
        ServiceError::InvalidToken { .. } => "error_reset_link_invalid",
        _ => {
            error!("Reset password failed: {e}");

            "error_internal"
        }
    }
}

/// POST /admin/reset-password — validate token, update password, redirect to login.
pub async fn reset_password_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ResetPasswordForm>,
) -> Response {
    let client = client_ip(&headers, &addr, &state.config.server);

    // Local-only validation runs FIRST, before the rate-limit gate: a
    // mismatched confirmation or a policy violation is a legitimate user typo,
    // not a token guess, so it must not consume the IP's budget.
    if form.password != form.password_confirm {
        return render_reset_error(&state, Some(&form.token), "error_passwords_no_match");
    }

    if let Err(violation) = state.config.auth.password_policy.validate(&form.password) {
        return render_policy_violation(&state, &form.token, violation);
    }

    // Rate limit by IP to prevent brute-forcing reset tokens. Atomically record
    // this attempt and bail if it puts the IP over the threshold — one backend
    // op, closing the check-then-record race the old `is_blocked` +
    // `record_failure` split left open. The gate sits AFTER the local checks
    // above so only genuine token-consumption attempts count. Every such attempt
    // counts (the same idiom as login and forgot-password), so a transient
    // internal error counts too — acceptable for a high-entropy token endpoint
    // and strictly safer than refunding. Uses its OWN per-IP keyspace (not the
    // shared forgot-password limiter) so reset-token attempts and the
    // forgot-password request flow don't drain each other's budget.
    let ip_reset_limiter = state
        .ip_forgot_password_limiter
        .rescoped(IP_RESET_PASSWORD_KEYSPACE);
    if ip_reset_limiter.check_and_block_ip(&client) {
        return render_reset_error(&state, Some(&form.token), "error_reset_link_invalid");
    }

    let infra = Arc::clone(&state.infra);
    let token = form.token.clone();
    let password = form.password.clone();

    let result =
        spawn_request_blocking(move || reset_password_blocking(&infra, &token, &password)).await;

    match result {
        Ok(Ok(())) => {
            Redirect::to(&paths::login_with_success("success_password_reset")).into_response()
        }
        Ok(Err(e)) => render_reset_error(&state, None, reset_error_key(&e)),
        Err(e) => {
            error!("Reset password task error: {}", e);

            render_reset_error(&state, None, "error_internal")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use anyhow::anyhow;

    use super::*;

    /// Regression: a failure that is not a token refusal (a refused commit, a
    /// backend error) read as "invalid link"; only token refusals do now.
    #[test]
    fn only_token_refusals_blame_the_link() {
        let expired = ServiceError::InvalidToken {
            kind: "reset",
            reason: "expired",
        };
        let missing = ServiceError::InvalidToken {
            kind: "reset",
            reason: "not found",
        };
        let late = ServiceError::Transient(anyhow!("deadline passed"));

        assert_eq!(reset_error_key(&expired), "error_reset_link_expired");
        assert_eq!(reset_error_key(&missing), "error_reset_link_invalid");
        assert_eq!(reset_error_key(&late), "error_internal");
    }

    /// Regression: a policy violation on the reset page rendered the English
    /// `Display` text whatever the page's locale; it renders through its
    /// translation key, params filled in.
    #[test]
    fn a_violation_renders_in_the_page_locale() {
        let translations = Translations::load(Path::new("/nonexistent"));

        let german =
            violation_message(&translations, "de", PasswordViolation::TooShort { min: 12 });
        assert_eq!(german, "Das Passwort muss mindestens 12 Zeichen lang sein");

        let english = violation_message(&translations, "en", PasswordViolation::MissingDigit);
        assert_eq!(english, "Password must contain at least one digit");
    }
}
