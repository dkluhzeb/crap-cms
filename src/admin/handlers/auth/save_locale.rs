use anyhow::Error;
use axum::{
    Extension,
    extract::{Form, State},
    http::StatusCode,
    response::IntoResponse,
};

use crate::{
    admin::{AdminState, handlers::auth::LocaleForm},
    core::{auth::AuthUser, spawn_request_blocking},
    db::DbPool,
    service::user_settings,
};

/// Store the user's preferred UI locale, leaving their other settings as
/// they are (see [`user_settings::update_user_settings`]).
fn update_user_locale(pool: &DbPool, user_id: &str, locale: &str) -> Result<(), Error> {
    user_settings::update_user_settings(pool, user_id, |settings| {
        settings.set_ui_locale(locale);
    })?;

    Ok(())
}

/// POST /admin/api/locale — save user's preferred admin UI locale.
pub async fn save_locale(
    State(state): State<AdminState>,
    Extension(auth_user): Extension<AuthUser>,
    Form(form): Form<LocaleForm>,
) -> impl IntoResponse {
    let available = state.translations.available_locales();

    if !available.contains(&form.locale.as_str()) {
        return StatusCode::BAD_REQUEST;
    }

    let pool = state.infra.pool.clone();
    let user_id = auth_user.claims.sub.clone();
    let locale = form.locale.clone();

    let result = spawn_request_blocking(move || update_user_locale(&pool, &user_id, &locale)).await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
