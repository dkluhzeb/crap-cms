use axum::{
    extract::{Form, State},
    response::{Html, IntoResponse, Redirect},
};

use crate::admin::AdminState;
use crate::admin::context::{ContextBuilder, PageType};
use crate::db::query;
use super::ResetPasswordForm;

/// POST /admin/reset-password — validate token, update password, redirect to login.
pub async fn reset_password_action(
    State(state): State<AdminState>,
    Form(form): Form<ResetPasswordForm>,
) -> axum::response::Response {
    if form.password != form.password_confirm {
        let data = ContextBuilder::auth(&state)
            .page(PageType::AuthReset, "Reset Password")
            .set("token", serde_json::json!(form.token))
            .set("error", serde_json::json!("error_passwords_no_match"))
            .build();
        return match state.render("auth/reset_password", &data) {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
        };
    }

    if let Err(e) = state.config.auth.password_policy.validate(&form.password) {
        let data = ContextBuilder::auth(&state)
            .page(PageType::AuthReset, "Reset Password")
            .set("token", serde_json::json!(form.token))
            .set("error", serde_json::json!(e.to_string()))
            .build();
        return match state.render("auth/reset_password", &data) {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
        };
    }

    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = form.token.clone();
    let password = form.password.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;

        // Search all auth collections for the token
        for def in registry.collections.values() {
            if !def.is_auth_collection() { continue; }
            if let Some((user, exp)) = query::find_by_reset_token(&conn, &def.slug, def, &token)? {
                if chrono::Utc::now().timestamp() >= exp {
                    query::clear_reset_token(&conn, &def.slug, &user.id)?;
                    return Err(anyhow::anyhow!("expired"));
                }
                // Update password and clear token
                query::update_password(&conn, &def.slug, &user.id, &password)?;
                query::clear_reset_token(&conn, &def.slug, &user.id)?;
                return Ok(());
            }
        }

        Err(anyhow::anyhow!("invalid_token"))
    }).await;

    match result {
        Ok(Ok(())) => {
            Redirect::to("/admin/login?success=success_password_reset").into_response()
        }
        Ok(Err(e)) => {
            let msg = if e.to_string().contains("expired") {
                "error_reset_link_expired"
            } else {
                "error_reset_link_invalid"
            };
            let data = ContextBuilder::auth(&state)
                .page(PageType::AuthReset, "Reset Password")
                .set("error", serde_json::json!(msg))
                .build();
            match state.render("auth/reset_password", &data) {
                Ok(html) => Html(html).into_response(),
                Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
            }
        }
        Err(e) => {
            tracing::error!("Reset password task error: {}", e);
            let data = ContextBuilder::auth(&state)
                .page(PageType::AuthReset, "Reset Password")
                .set("error", serde_json::json!("error_internal"))
                .build();
            match state.render("auth/reset_password", &data) {
                Ok(html) => Html(html).into_response(),
                Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
            }
        }
    }
}
