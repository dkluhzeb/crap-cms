use anyhow::Error;
use axum::{
    Extension,
    extract::{Form, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, from_str, json, to_string};
use tokio::task;

use super::LocaleForm;
use crate::{admin::AdminState, core::auth::AuthUser, db::query};

/// POST /admin/api/locale — save user's preferred admin UI locale.
pub async fn save_locale(
    State(state): State<AdminState>,
    Extension(auth_user): Extension<AuthUser>,
    Form(form): Form<LocaleForm>,
) -> impl IntoResponse {
    // Validate locale is available
    let available = state.translations.available_locales();

    if !available.contains(&form.locale.as_str()) {
        return StatusCode::BAD_REQUEST;
    }

    let pool = state.pool.clone();
    let user_id = auth_user.claims.sub.clone();
    let locale = form.locale.clone();

    let result = task::spawn_blocking(move || {
        let conn = pool.get()?;
        let existing = query::get_user_settings(&conn, &user_id)?;
        let mut settings: Value = existing
            .as_deref()
            .and_then(|s| from_str(s).ok())
            .unwrap_or_else(|| json!({}));

        settings["ui_locale"] = json!(locale);

        let json_str = to_string(&settings)?;

        query::set_user_settings(&conn, &user_id, &json_str)?;

        Ok::<_, Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
