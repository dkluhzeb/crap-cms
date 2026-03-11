use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect},
};

use crate::admin::AdminState;
use crate::db::query;
use super::VerifyEmailQuery;

/// GET /admin/verify-email?token=xxx — validate token, mark verified, redirect.
pub async fn verify_email(
    State(state): State<AdminState>,
    Query(query): Query<VerifyEmailQuery>,
) -> axum::response::Response {
    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token;

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;

        for def in registry.collections.values() {
            if !def.is_auth_collection() { continue; }
            if !def.auth.as_ref().is_some_and(|a| a.verify_email) { continue; }
            if let Some((user, exp)) = query::find_by_verification_token(&conn, &def.slug, def, &token)? {
                if chrono::Utc::now().timestamp() >= exp {
                    // Token expired — don't verify
                    return Ok(false);
                }
                query::mark_verified(&conn, &def.slug, &user.id)?;
                return Ok(true);
            }
        }

        Ok::<_, anyhow::Error>(false)
    }).await;

    match result {
        Ok(Ok(true)) => {
            Redirect::to("/admin/login?success=success_email_verified").into_response()
        }
        _ => {
            Redirect::to("/admin/login").into_response()
        }
    }
}
