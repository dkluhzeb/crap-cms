//! Validation-only endpoint for globals.
//!
//! Runs the full before_validate → validate pipeline inside a rolled-back transaction,
//! returning JSON `{ valid: true }` or `{ valid: false, errors: { ... } }`.

use axum::{
    Extension, Json,
    extract::{Path, State},
    response::Response,
};
use tokio::task;

use crate::{
    admin::{
        AdminState,
        handlers::{
            forms::FormData,
            shared::{check_access_or_forbid, get_user_doc, parse_request_locale},
            validate::{
                RunValidationParams, ValidateRequest, handle_validation_result, run_validation,
                validation_error_response_simple, values_to_string_map,
            },
        },
    },
    core::{DocumentFields, auth::AuthUser},
    db::{AccessResult, query::helpers::global_table},
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

    let form_data = values_to_string_map(&payload.data);

    // Field write access stripping is now handled inside service::validate_document
    // via WriteHooks::field_write_denied.

    let data: DocumentFields = FormData::from_raw(form_data, &def.fields).into();

    let is_draft = payload.draft && def.has_drafts();
    let locale_ctx = match parse_request_locale(payload.locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return validation_error_response_simple(&msg),
    };

    let gtable = global_table(&slug);
    let pool = state.infra.pool.clone();
    let runner = state.infra.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc = get_user_doc(auth_user.as_ref()).cloned();

    let result = task::spawn_blocking(move || {
        run_validation(&RunValidationParams {
            pool: &pool,
            runner: &runner,
            hooks: &def_owned.hooks,
            fields: &def_owned.fields,
            slug: &slug_owned,
            table_name: &gtable,
            operation: "update",
            exclude_id: Some("default"),
            data: &data,
            is_draft,
            soft_delete: false,
            supports_drafts: def_owned.has_drafts(),
            locale_ctx: locale_ctx.as_ref(),
            user_doc: user_doc.as_ref(),
            required_locales: None,
        })
    })
    .await;

    handle_validation_result(result, auth_user.as_ref(), &state)
}
