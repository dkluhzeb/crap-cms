use anyhow::Error;
use axum::{
    Extension,
    extract::{Form, State},
    http::StatusCode,
    response::IntoResponse,
};
use tokio::task;

use crate::{
    admin::{AdminState, handlers::auth::LocaleForm},
    core::auth::AuthUser,
    db::DbPool,
    service::user_settings,
};

/// Read the user's settings JSON, update the `ui_locale` field, and write it back.
fn update_user_locale(pool: &DbPool, user_id: &str, locale: &str) -> Result<(), Error> {
    // IMMEDIATE tx: same whole-blob read-modify-write lost-update guard as
    // `save_column_preferences` — a concurrent column-preference save must not
    // clobber this locale change. From the write pool, so the transaction does
    // not hold a read connection.
    let mut conn = pool.write()?;
    let tx = conn.transaction_immediate()?;

    let mut settings = user_settings::load_user_settings(&tx, user_id)?;
    settings.set_ui_locale(locale);

    user_settings::set_user_settings(&tx, user_id, &settings.to_json())?;
    tx.commit()?;

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

    let result = task::spawn_blocking(move || update_user_locale(&pool, &user_id, &locale)).await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
