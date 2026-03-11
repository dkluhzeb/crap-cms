use anyhow::Context as _;
use anyhow::Error;
use axum::{
    Extension, Form,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, from_str, json, to_string};
use std::collections::HashMap;
use tokio::task;

use crate::{
    admin::{AdminState, handlers::shared::is_column_eligible},
    core::auth::AuthUser,
    db::query,
};

/// POST /admin/api/user-settings/{slug} — save user column preferences
pub async fn save_user_settings(
    State(state): State<AdminState>,
    Path(collection_slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let auth_user = match auth_user {
        Some(Extension(au)) => au,
        None => return StatusCode::UNAUTHORIZED,
    };

    // Validate collection exists
    let def = match state.registry.get_collection(&collection_slug) {
        Some(d) => d.clone(),
        None => return StatusCode::NOT_FOUND,
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
    let result = task::spawn_blocking(move || {
        let conn = pool.get().context("Failed to get DB connection")?;
        let existing = query::auth::get_user_settings(&conn, &user_id)?;
        let mut settings: Value = existing
            .as_deref()
            .and_then(|s| from_str(s).ok())
            .unwrap_or_else(|| json!({}));

        settings[&collection_slug] = json!({ "columns": valid_columns });

        let json_str = to_string(&settings)?;

        query::auth::set_user_settings(&conn, &user_id, &json_str)?;

        Ok::<_, Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
