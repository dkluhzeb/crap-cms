use axum::{
    extract::{Query, State},
    response::Response,
};
use tokio::task;

use crate::{
    admin::{
        AdminState,
        context::{AuthBasePageContext, PageMeta, PageType, page::auth::ResetPasswordPage},
        handlers::{auth::ResetPasswordQuery, shared::render_page},
    },
    core::Registry,
    db::DbPool,
    service::{self, ServiceContext},
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

        let ctx = ServiceContext::collection(&def.slug, def)
            .conn(&conn)
            .build();

        if service::auth::find_by_reset_token(&ctx, token).unwrap_or(false) {
            return true;
        }
    }

    false
}

/// GET /admin/reset-password?token=xxx — validate token, show reset form.
pub async fn reset_password_page(
    State(state): State<AdminState>,
    Query(query): Query<ResetPasswordQuery>,
) -> Response {
    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token.clone();

    let valid = task::spawn_blocking(move || is_valid_reset_token(&pool, &registry, &token))
        .await
        .unwrap_or(false);

    let ctx = ResetPasswordPage {
        base: AuthBasePageContext::for_state(
            &state,
            PageMeta::new(PageType::AuthReset, "reset_password_page_title"),
        ),
        token: valid.then(|| query.token.clone()),
        error: (!valid).then(|| "error_reset_link_invalid".to_string()),
    };

    render_page(&state, "auth/reset_password", &ctx)
}
