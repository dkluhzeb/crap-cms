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
            shared::{get_user_doc, parse_request_locale},
            validate::{
                ValidateRequest, handle_validation_outcome, validation_error_response_simple,
                values_to_string_map,
            },
        },
    },
    core::{DocumentFields, auth::AuthUser},
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

    // Field write access stripping is now handled inside service::validate_document
    // via WriteHooks::field_write_denied.

    let data: DocumentFields = FormData::from_raw(form_data, &def.fields).into();

    let locale_ctx = match parse_request_locale(payload.locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return validation_error_response_simple(&msg),
    };

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

    handle_validation_outcome(result, auth_user.as_ref(), &state)
}
