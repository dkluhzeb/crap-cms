use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            EvaluateConditionsRequest, check_access_or_forbid, evaluate_condition_results,
            get_user_doc,
        },
    },
    core::auth::AuthUser,
    db::AccessResult,
    hooks::ConditionContext,
};

/// POST /admin/collections/{slug}/evaluate-conditions
/// Evaluates server-only display conditions with current form data.
/// Returns JSON: `{ "field_name": true/false, ... }`
pub(crate) async fn evaluate_conditions(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(req): Json<EvaluateConditionsRequest>,
) -> impl IntoResponse {
    let Some(def) = state.infra.registry.get_collection(&slug) else {
        return (StatusCode::NOT_FOUND, Json(json!({}))).into_response();
    };

    match check_access_or_forbid(
        &state,
        def.access.read.as_ref(),
        auth_user.as_ref(),
        None,
        None,
        "read",
        &slug,
    ) {
        Ok(AccessResult::Denied) | Err(_) => {
            return (StatusCode::FORBIDDEN, Json(json!({}))).into_response();
        }
        _ => {}
    }

    let cond_ctx = ConditionContext {
        collection: &slug,
        operation: &req.operation,
        user: get_user_doc(auth_user.as_ref()),
        ui_locale: auth_user
            .as_ref()
            .map(|Extension(au)| au.ui_locale.as_str()),
        locale: None,
        options: None,
    };

    let results =
        evaluate_condition_results(&state.infra.hook_runner, &def.fields, &req, &cond_ctx);

    Json(Value::Object(results)).into_response()
}
