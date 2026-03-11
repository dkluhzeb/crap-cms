use anyhow::Context as _;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension,
};
use std::collections::HashMap;

use crate::admin::handlers::shared::is_column_eligible;
use crate::admin::AdminState;
use crate::core::auth::AuthUser;
use crate::db::query;

/// POST /admin/api/user-settings/{slug} — save user column preferences
pub async fn save_user_settings(
    State(state): State<AdminState>,
    Path(collection_slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let auth_user = match auth_user {
        Some(Extension(au)) => au,
        None => return axum::http::StatusCode::UNAUTHORIZED.into_response(),
    };

    // Validate collection exists
    let def = match state.registry.get_collection(&collection_slug) {
        Some(d) => d.clone(),
        None => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };

    // Parse columns from form (comma-separated or multiple "columns" fields)
    let columns: Vec<String> = form
        .get("columns")
        .map(|c| {
            c.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    // Validate column keys
    let valid_columns: Vec<String> = columns
        .into_iter()
        .filter(|k| {
            k == "created_at"
                || k == "updated_at"
                || k == "_status"
                || def
                    .fields
                    .iter()
                    .any(|f| f.name == *k && is_column_eligible(&f.field_type))
        })
        .collect();

    // Load existing settings, merge, save
    let pool = state.pool.clone();
    let user_id = auth_user.claims.sub.clone();
    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get().context("Failed to get DB connection")?;
        let existing = query::auth::get_user_settings(&conn, &user_id)?;
        let mut settings: serde_json::Value = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        settings[&collection_slug] = serde_json::json!({ "columns": valid_columns });

        let json_str = serde_json::to_string(&settings)?;
        query::auth::set_user_settings(&conn, &user_id, &json_str)?;
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => axum::http::StatusCode::NO_CONTENT.into_response(),
        _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
