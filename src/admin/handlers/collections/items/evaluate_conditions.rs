use std::borrow::Cow;

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
            EvaluateConditionsRequest, check_access_or_forbid, collection_form_fields,
            evaluate_condition_results, get_user_doc,
        },
    },
    core::{CollectionDefinition, FieldDefinition, auth::AuthUser},
    db::AccessResult,
    hooks::ConditionContext,
};

/// The fields the form rendered for its viewer: an edit form's, judged like its
/// submission is; a create form's are the declared ones.
async fn rendered_fields<'a>(
    state: &AdminState,
    def: &'a CollectionDefinition,
    req: &EvaluateConditionsRequest,
    auth_user: Option<&Extension<AuthUser>>,
) -> Cow<'a, [FieldDefinition]> {
    let Some(id) = req.document_id.as_deref() else {
        return Cow::Borrowed(&def.fields);
    };

    collection_form_fields(state, def, id, auth_user, req.locale.as_deref()).await
}

/// POST /admin/collections/{slug}/evaluate-conditions
/// Evaluates server-only display conditions with current form data.
/// Returns JSON: `{ "field_name": true/false, ... }`
pub(crate) async fn evaluate_conditions(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(req): Json<EvaluateConditionsRequest>,
) -> impl IntoResponse {
    let Some(def) = state.infra.registry.get_collection(&slug).cloned() else {
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

    let form_fields = rendered_fields(&state, &def, &req, auth_user.as_ref()).await;

    let cond_ctx = ConditionContext {
        collection: &slug,
        operation: &req.operation,
        user: get_user_doc(auth_user.as_ref()),
        ui_locale: auth_user
            .as_ref()
            .map(|Extension(au)| au.ui_locale.as_str()),
        locale: req.locale.as_deref(),
        options: None,
    };

    let results =
        evaluate_condition_results(&state.infra.hook_runner, &form_fields, &req, &cond_ctx);

    Json(Value::Object(results)).into_response()
}
