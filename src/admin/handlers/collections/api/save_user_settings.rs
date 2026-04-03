use std::collections::HashMap;

use anyhow::{Context as _, Error};
use axum::{
    Extension, Form,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, from_str, json, to_string};
use tokio::task;

use crate::{
    admin::{AdminState, handlers::shared::is_column_eligible},
    core::auth::AuthUser,
    db::{DbPool, query},
};

/// Parse and validate column keys from the form against the collection definition.
fn parse_valid_columns(
    form: &HashMap<String, String>,
    def: &crate::core::CollectionDefinition,
) -> Vec<String> {
    let columns: Vec<String> = form
        .get("columns")
        .map(|c| {
            c.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    columns
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
        .collect()
}

/// Load the user's settings JSON, merge column preferences for one collection, and save.
fn save_column_preferences(
    pool: &DbPool,
    user_id: &str,
    collection_slug: &str,
    columns: Vec<String>,
) -> Result<(), Error> {
    let conn = pool.get().context("Failed to get DB connection")?;
    let existing = query::auth::get_user_settings(&conn, user_id)?;

    let mut settings: Value = existing
        .as_deref()
        .and_then(|s| from_str(s).ok())
        .unwrap_or_else(|| json!({}));

    settings[collection_slug] = json!({ "columns": columns });

    let json_str = to_string(&settings)?;

    query::auth::set_user_settings(&conn, user_id, &json_str)?;

    Ok(())
}

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

    let def = match state.registry.get_collection(&collection_slug) {
        Some(d) => d.clone(),
        None => return StatusCode::NOT_FOUND,
    };

    let valid_columns = parse_valid_columns(&form, &def);
    let pool = state.pool.clone();
    let user_id = auth_user.claims.sub.clone();

    let result = task::spawn_blocking(move || {
        save_column_preferences(&pool, &user_id, &collection_slug, valid_columns)
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
