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
            shared::{get_user_doc, parse_request_locale},
            validate::{
                ValidateRequest, handle_validation_outcome, validation_error_response_simple,
            },
        },
    },
    core::auth::AuthUser,
    service::op::{self, Principal, TargetRef, Validate, ValidateArgs},
};

use super::helpers::prepare_form_for_validation;

/// POST /admin/collections/{slug}/validate — validate fields for create
#[tracing::instrument(skip(state, auth_user, payload), name = "collections::validate_create")]
pub async fn validate_create(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(payload): Json<ValidateRequest>,
) -> Response {
    let Some(def) = state.infra.registry.get_collection(&slug).cloned() else {
        return validation_error_response_simple("Collection not found");
    };

    // Collection-level access is enforced in the shared operation body —
    // same rule, same user as the real write.

    let data = prepare_form_for_validation(&state, &def, auth_user.as_ref(), &payload, "create");

    let locale_ctx = match parse_request_locale(payload.locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return validation_error_response_simple(&msg),
    };

    // Shared dry-run body: rolled-back transaction, field-access stripping as
    // the resolved editor, draft clamp — identical on every surface.
    let args = ValidateArgs::builder(data)
        .locale_ctx(locale_ctx)
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
