use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::{Value, json};
use tracing::warn;

use crate::{
    admin::{
        AdminState,
        handlers::shared::{
            EvaluateConditionsRequest, check_access_or_forbid, evaluate_condition_results,
            get_user_doc,
        },
    },
    core::{GlobalDefinition, auth::AuthUser},
    hooks::ConditionContext,
    service::global_access_allowed,
};

/// Whether the caller may read global `slug` — mapped as every global
/// surface maps it: a filter table is a configuration error, refused like a
/// denial rather than read as an allow.
fn read_allowed(
    state: &AdminState,
    def: &GlobalDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    slug: &str,
) -> bool {
    let Ok(access) = check_access_or_forbid(
        state,
        def.access.read.as_ref(),
        auth_user,
        None,
        None,
        "read",
        slug,
    ) else {
        return false;
    };

    global_access_allowed(&access, slug)
        .inspect_err(|e| warn!("Global condition evaluation for '{slug}': {e}"))
        .unwrap_or(false)
}

/// POST /admin/globals/{slug}/evaluate-conditions
/// Evaluates server-only display conditions with current form data.
/// Returns JSON: `{ "field_name": true/false, ... }`
pub(crate) async fn evaluate_conditions(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(req): Json<EvaluateConditionsRequest>,
) -> impl IntoResponse {
    let Some(def) = state.infra.registry.get_global(&slug) else {
        return (StatusCode::NOT_FOUND, Json(json!({}))).into_response();
    };

    if !read_allowed(&state, def, auth_user.as_ref(), &slug) {
        return (StatusCode::FORBIDDEN, Json(json!({}))).into_response();
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
