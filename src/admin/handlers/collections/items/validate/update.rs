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
            shared::{
                ErrorLabels, get_user_doc, parse_request_locale, strip_locale_locked_form_fields,
            },
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

    // Collection-level access is enforced in the shared operation body —
    // same rule, same user as the real write.

    let locale_ctx = match parse_request_locale(payload.locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return validation_error_response_simple(&msg),
    };

    // The edit form echoes shared fields read-only under a non-default locale;
    // the admin write drops them before the service's locale lock, so the
    // dry-run it previews does too.
    let data = strip_locale_locked_form_fields(
        prepare_form_for_validation(&state, &def, auth_user.as_ref(), &payload, "update"),
        &def.fields,
        locale_ctx.as_ref(),
    );

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

    let form = values_to_string_map(&payload.data);
    let labels = ErrorLabels::new(&def.fields, Some(&form));

    handle_validation_outcome(result, auth_user.as_ref(), &state, &labels)
}
