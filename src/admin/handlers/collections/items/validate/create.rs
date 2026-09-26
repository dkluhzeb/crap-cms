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
            shared::{ErrorLabels, get_user_doc, parse_request_locale},
            validate::{
                ValidateRequest, handle_validation_outcome, validation_error_response_simple,
                values_to_string_map,
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

    let data = prepare_form_for_validation(&def, &def.fields, &payload);

    let locale_ctx = match parse_request_locale(payload.locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return validation_error_response_simple(&msg),
    };

    // Shared dry-run body: rolled-back transaction, field-access stripping as
    // the resolved editor, draft clamp — identical on every surface.
    // An upload collection's admin create is the multipart path, whose real
    // write carries the server-derived metadata the placeholders above stand
    // in for — so the dry-run keeps them, like that trusted write.
    let args = ValidateArgs::builder(data)
        .locale_ctx(locale_ctx)
        .draft(payload.draft)
        .trusted_upload_metadata(def.is_upload_collection())
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

    let form = values_to_string_map(&payload.data);
    let labels = ErrorLabels::new(&def.fields, Some(&form));

    handle_validation_outcome(result, auth_user.as_ref(), &state, &labels)
}
