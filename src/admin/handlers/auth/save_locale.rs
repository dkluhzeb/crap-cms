use axum::{
    extract::{Form, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};

use super::LocaleForm;
use crate::admin::AdminState;
use crate::core::auth::AuthUser;
use crate::db::query;

/// POST /admin/api/locale — save user's preferred admin UI locale.
pub async fn save_locale(
    State(state): State<AdminState>,
    Extension(auth_user): Extension<AuthUser>,
    Form(form): Form<LocaleForm>,
) -> impl IntoResponse {
    // Validate locale is available
    let available = state.translations.available_locales();
    if !available.contains(&form.locale.as_str()) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let pool = state.pool.clone();
    let user_id = auth_user.claims.sub.clone();
    let locale = form.locale.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let existing = query::get_user_settings(&conn, &user_id)?;
        let mut settings: serde_json::Value = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        settings["ui_locale"] = serde_json::json!(locale);

        let json_str = serde_json::to_string(&settings)?;
        query::set_user_settings(&conn, &user_id, &json_str)?;
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
