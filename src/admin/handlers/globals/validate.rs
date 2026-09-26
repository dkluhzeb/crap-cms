//! Validation-only endpoint for globals.
//!
//! Runs the full before_validate → validate pipeline inside a rolled-back transaction,
//! returning JSON `{ valid: true }` or `{ valid: false, errors: { ... } }`.

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
            forms::FormData,
            shared::{
                ErrorLabels, get_user_doc, global_form_fields, parse_request_locale,
                strip_locale_locked_form_fields,
            },
            validate::{
                ValidateRequest, handle_validation_outcome, validation_error_response_simple,
                values_to_string_map,
            },
        },
    },
    core::auth::AuthUser,
    service::op::{self, Principal, TargetRef, ValidateArgs, ValidateGlobal},
};

/// POST /admin/globals/{slug}/validate — validate fields for global update
#[tracing::instrument(skip(state, auth_user, payload), name = "globals::validate_global")]
pub async fn validate_global(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(payload): Json<ValidateRequest>,
) -> Response {
    let def = match state.infra.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return validation_error_response_simple("Global not found"),
    };

    // Collection-level access is enforced in the shared operation body —
    // same rule, same user as the real write.

    let form_data = values_to_string_map(&payload.data);

    // Field write access stripping is handled inside the shared operation body.

    let locale_ctx = match parse_request_locale(payload.locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return validation_error_response_simple(&msg),
    };

    // Parsed against the fields the edit form rendered for this viewer, as its
    // save is. The form echoes shared fields read-only under a non-default
    // locale; the admin write drops them before the service's locale lock, so
    // the dry-run it previews does too.
    let form_fields =
        global_form_fields(&state, &def, auth_user.as_ref(), payload.locale.as_deref()).await;
    let data = strip_locale_locked_form_fields(
        FormData::from_raw(form_data.clone(), &form_fields).into(),
        &def.fields,
        locale_ctx.as_ref(),
    );

    // Shared dry-run body — globals always validate as an update against the
    // singleton `default` row of `_global_<slug>`.
    let args = ValidateArgs::builder(data)
        .locale_ctx(locale_ctx)
        .draft(payload.draft)
        .build();

    let result = op::run_blocking::<ValidateGlobal>(
        Arc::clone(&state.infra),
        Principal::Resolved {
            user: get_user_doc(auth_user.as_ref()).cloned(),
            ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
        },
        TargetRef::global(slug),
        args,
    )
    .await;

    let labels = ErrorLabels::new(&def.fields, Some(&form_data));

    handle_validation_outcome(result, auth_user.as_ref(), &state, &labels)
}
