//! MFA (Multi-Factor Authentication) handlers for the admin UI.

use axum::{
    extract::{Form, Query, State},
    http::{HeaderMap, header::COOKIE},
    response::{Html, IntoResponse, Redirect, Response},
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::auth::{
            MfaForm, MfaQuery, append_cookies, clear_mfa_pending_cookie, create_session_token,
            session_redirect,
        },
        server::extract_cookie,
    },
    core::auth::Claims,
    service,
};

/// Extract the `crap_mfa_pending` cookie value from request headers.
fn extract_mfa_token(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(COOKIE)?.to_str().ok()?;

    extract_cookie(cookie_header, "crap_mfa_pending").map(|s| s.to_string())
}

/// Render the MFA code entry form with an optional error message.
fn render_mfa_form(state: &AdminState, error: Option<&str>) -> Response {
    let mut builder = ContextBuilder::auth(state).page(PageType::AuthMfa, "mfa_page_title");

    if let Some(err) = error {
        builder = builder.set("error", json!(err));
    }

    let data = builder.build();

    match state.render("auth/mfa", &data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("MFA template render error: {}", e);

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
                .into_response()
        }
    }
}

/// GET /admin/mfa — show the MFA code entry form.
pub async fn mfa_page(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(_query): Query<MfaQuery>,
) -> Response {
    // If there's no pending MFA cookie, redirect to login
    if extract_mfa_token(&headers).is_none() {
        return Redirect::to("/admin/login").into_response();
    }

    render_mfa_form(&state, None)
}

/// POST /admin/mfa — verify the MFA code and complete login.
pub async fn verify_mfa_action(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Form(form): Form<MfaForm>,
) -> Response {
    // Extract and validate the MFA pending token
    let mfa_token = match extract_mfa_token(&headers) {
        Some(t) => t,
        None => return Redirect::to("/admin/login").into_response(),
    };

    let pending_claims = match state.token_provider.validate_token(&mfa_token) {
        Ok(c) => c,
        Err(_) => {
            // Token expired or invalid — clear cookie, redirect to login
            let cookie = clear_mfa_pending_cookie(state.config.admin.dev_mode);
            let mut response = Redirect::to("/admin/login").into_response();

            append_cookies(&mut response, &[cookie]);

            return response;
        }
    };

    // Verify the MFA code against the database
    let pool = state.pool.clone();
    let slug = pending_claims.collection.to_string();
    let user_id = pending_claims.sub.to_string();
    let code = form.code.clone();

    let verify_result = task::spawn_blocking(move || {
        let conn = pool.get()?;
        service::auth::verify_mfa_code(&conn, &slug, &user_id, &code).map_err(|e| e.into_anyhow())
    })
    .await;

    let verified = match verify_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            error!("MFA verification error: {}", e);

            return render_mfa_form(&state, Some("error_internal"));
        }
        Err(e) => {
            error!("MFA verification task error: {}", e);

            return render_mfa_form(&state, Some("error_internal"));
        }
    };

    if !verified {
        return render_mfa_form(&state, Some("error_mfa_invalid_code"));
    }

    // MFA verified — build full session
    build_mfa_session_response(&state, &pending_claims)
}

/// Build the final session response after successful MFA verification.
fn build_mfa_session_response(state: &AdminState, pending: &Claims) -> Response {
    let session = match create_session_token(
        state,
        pending.sub.to_string(),
        &pending.collection,
        pending.email.clone(),
        pending.session_version,
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("MFA session: {}", e);
            return render_mfa_form(state, Some("error_internal"));
        }
    };

    let mut response = session_redirect(&session, state.config.admin.dev_mode);

    append_cookies(
        &mut response,
        &[clear_mfa_pending_cookie(state.config.admin.dev_mode)],
    );

    response
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
}
