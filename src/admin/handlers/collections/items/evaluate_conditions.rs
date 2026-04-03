use axum::{
    Extension, Json,
    extract::{Path, State},
    response::IntoResponse,
};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            EvaluateConditionsRequest, check_access_or_forbid, evaluate_condition_results,
        },
    },
    core::auth::AuthUser,
    db::AccessResult,
};

/// POST /admin/collections/{slug}/evaluate-conditions
/// Evaluates server-only display conditions with current form data.
/// Returns JSON: { "field_name": true/false, ... }
pub(crate) async fn evaluate_conditions(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(req): Json<EvaluateConditionsRequest>,
) -> impl IntoResponse {
    let Some(def) = state.registry.get_collection(&slug) else {
        return Json(json!({})).into_response();
    };

    match check_access_or_forbid(&state, def.access.read.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) | Err(_) => return Json(json!({})).into_response(),
        _ => {}
    }

    let results = evaluate_condition_results(&state.hook_runner, &def.fields, &req);

    Json(Value::Object(results)).into_response()
}
