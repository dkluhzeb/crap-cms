use axum::{
    extract::{Query, State},
    response::Html,
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::auth::ResetPasswordQuery,
    },
    core::Registry,
    db::DbPool,
    service,
};

/// Check whether a reset token exists across all auth collections.
fn is_valid_reset_token(pool: &DbPool, registry: &Registry, token: &str) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };

    for def in registry.collections.values() {
        if !def.is_auth_collection() {
            continue;
        }

        if service::auth::find_by_reset_token(&conn, &def.slug, def, token).unwrap_or(false) {
            return true;
        }
    }

    false
}

/// GET /admin/reset-password?token=xxx — validate token, show reset form.
pub async fn reset_password_page(
    State(state): State<AdminState>,
    Query(query): Query<ResetPasswordQuery>,
) -> Html<String> {
    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token.clone();

    let valid = task::spawn_blocking(move || is_valid_reset_token(&pool, &registry, &token))
        .await
        .unwrap_or(false);

    let mut builder =
        ContextBuilder::auth(&state).page(PageType::AuthReset, "reset_password_page_title");

    if valid {
        builder = builder.set("token", json!(query.token));
    } else {
        builder = builder.set("error", json!("error_reset_link_invalid"));
    }

    let data = builder.build();
    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/reset_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            error!("Template render error: {}", e);

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}
