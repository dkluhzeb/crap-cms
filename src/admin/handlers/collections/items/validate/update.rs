use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    response::Response,
};

use crate::{
    admin::{
        AdminState,
        handlers::{
            shared::{check_access_or_forbid, get_user_doc, parse_request_locale},
            validate::{
                ValidateRequest, handle_validation_outcome, validation_error_response_simple,
            },
        },
    },
    core::auth::AuthUser,
    db::AccessResult,
    service::op::{self, Principal, TargetRef, Validate, ValidateArgs},
};

use super::helpers::prepare_form_for_validation;

/// POST /admin/collections/{slug}/{id}/validate — validate fields for update
#[tracing::instrument(skip(state, auth_user, payload), name = "collections::validate_update")]
pub async fn validate_update(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
    Json(payload): Json<ValidateRequest>,
) -> Response {
    let Some(def) = state.infra.registry.get_collection(&slug).cloned() else {
        return validation_error_response_simple("Collection not found");
    };

    match check_access_or_forbid(
        &state,
        def.access.update.as_ref(),
        auth_user.as_ref(),
        None,
        None,
        "update",
        &slug,
    ) {
        Ok(AccessResult::Denied) => return validation_error_response_simple("Access denied"),
        Err(_) => return validation_error_response_simple("Access check failed"),
        _ => {}
    }

    let data = prepare_form_for_validation(&state, &def, auth_user.as_ref(), &payload, "update");

    let locale_ctx = match parse_request_locale(payload.locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return validation_error_response_simple(&msg),
    };

    // Shared dry-run body — `exclude_id` selects update mode (the target row
    // is excluded from unique checks).
    let args = ValidateArgs::builder(data)
        .locale_ctx(locale_ctx)
        .exclude_id(Some(id))
        .draft(payload.draft)
        .build();

    let result = op::run_blocking::<Validate>(
        Arc::clone(&state.infra),
        Principal::Resolved {
            user: get_user_doc(auth_user.as_ref()).cloned(),
            ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
        },
        TargetRef::collection(slug),
        args,
    )
    .await;

    handle_validation_outcome(result, auth_user.as_ref(), &state)
}
