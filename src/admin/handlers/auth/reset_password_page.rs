use axum::{
    extract::{Query, State},
    response::Html,
};

use super::ResetPasswordQuery;
use crate::admin::context::{ContextBuilder, PageType};
use crate::admin::AdminState;
use crate::db::query;

/// GET /admin/reset-password?token=xxx — validate token, show reset form.
pub async fn reset_password_page(
    State(state): State<AdminState>,
    Query(query): Query<ResetPasswordQuery>,
) -> Html<String> {
    // Validate the token exists and isn't expired
    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token.clone();

    let valid = tokio::task::spawn_blocking(move || {
        let conn = match pool.get() {
            Ok(c) => c,
            Err(_) => return false,
        };
        for def in registry.collections.values() {
            if !def.is_auth_collection() {
                continue;
            }
            match query::find_by_reset_token(&conn, &def.slug, def, &token) {
                Ok(Some((_, exp))) => {
                    return chrono::Utc::now().timestamp() < exp;
                }
                _ => continue,
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    let mut builder = ContextBuilder::auth(&state).page(PageType::AuthReset, "Reset Password");

    if valid {
        builder = builder.set("token", serde_json::json!(query.token));
    } else {
        builder = builder.set("error", serde_json::json!("error_reset_link_invalid"));
    }

    let data = builder.build();
    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/reset_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
