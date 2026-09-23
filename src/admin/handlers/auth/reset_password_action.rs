use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use tokio::task;
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
    core::{
        Registry, SharedInvalidationTransport, collection::Auth,
        rate_limit::IP_RESET_PASSWORD_KEYSPACE,
    },
    db::DbPool,
    service::{
        ServiceContext, ServiceError, auth::consume_reset_token as service_consume_reset_token,
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

/// Find the reset token across all auth collections, validate it, and update the password.
///
/// Searches every auth collection (with local auth enabled) for the token.
/// On success the password is updated and the token cleared inside a transaction.
///
/// On success the user's open live-update streams are torn down POST-COMMIT (a
/// password reset is a privilege-revoking action and an already-connected stream
/// never makes another request to pick up the bumped `_session_version`).
/// Publishing after commit ensures a rolled-back reset never spuriously tears
/// down a stream.
fn consume_reset_token(
    pool: &DbPool,
    registry: &Registry,
    token: &str,
    password: &str,
    invalidation_transport: &SharedInvalidationTransport,
) -> Result<(), ServiceError> {
    let mut conn = pool.write()?;
    // SELECT-then-UPDATE (find token row, then write the new hash): take a write
    // lock up front. A DEFERRED tx would risk `SQLITE_BUSY_SNAPSHOT` under
    // concurrent writers — same reasoning as the gRPC reset path.
    let tx = conn.transaction_immediate()?;

    for def in registry.collections.values() {
        if !def.is_auth_collection() {
            continue;
        }

        if !def.auth.as_ref().is_some_and(Auth::password_login_enabled) {
            continue;
        }

        let ctx = ServiceContext::collection(&def.slug, def).conn(&tx).build();

        match service_consume_reset_token(&ctx, token, password) {
            Ok(user_id) => {
                tx.commit()?;
                // Tear down the user's open live-update streams POST-COMMIT.
                ServiceContext::slug_only(&def.slug)
                    .invalidation_transport(Some(invalidation_transport.clone()))
                    .build()
                    .publish_user_invalidation(&user_id);
                return Ok(());
            }
            Err(ServiceError::InvalidToken {
                reason: "not found",
                ..
            }) => {}
            Err(e) => {
                tx.commit()?;
                return Err(e);
            }
        }
    }

    Err(ServiceError::InvalidToken {
        kind: "reset",
        reason: "not found",
    })
}

/// POST /admin/reset-password — validate token, update password, redirect to login.
pub async fn reset_password_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ResetPasswordForm>,
) -> Response {
    let ip = client_ip(&headers, &addr, &state.config.server);

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
    if ip_reset_limiter.check_and_block(&ip) {
        return render_reset_error(&state, Some(&form.token), "error_reset_link_invalid");
    }

    let pool = state.infra.pool.clone();
    let registry = Arc::clone(&state.infra.registry);
    let token = form.token.clone();
    let password = form.password.clone();
    let invalidation_transport = state.infra.invalidation_transport.clone();

    let result = task::spawn_blocking(move || {
        consume_reset_token(&pool, &registry, &token, &password, &invalidation_transport)
    })
    .await;

    match result {
        Ok(Ok(())) => {
            Redirect::to(&paths::login_with_success("success_password_reset")).into_response()
        }
        Ok(Err(e)) => {
            // Match the TYPED refusal: the service returns
            // `InvalidToken { reason }`, and flattening it to a string here
            // meant the expired branch could never be reached, so a dead link
            // always read as "invalid".
            let msg = match e {
                ServiceError::InvalidToken {
                    reason: "expired", ..
                } => "error_reset_link_expired",
                _ => "error_reset_link_invalid",
            };

            render_reset_error(&state, None, msg)
        }
        Err(e) => {
            error!("Reset password task error: {}", e);

            render_reset_error(&state, None, "error_internal")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
