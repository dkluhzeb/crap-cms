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
            forms::{extract_join_data_from_form, transform_select_has_many},
            shared::{check_access_or_forbid, get_user_doc},
            validate::{
                RunValidationParams, ValidateRequest, handle_validation_result, run_validation,
                validation_error_response_simple, values_to_string_map,
            },
        },
    },
    core::auth::AuthUser,
    db::{
        AccessResult,
        query::{LocaleContext, helpers::global_table},
    },
};

/// POST /admin/globals/{slug}/validate — validate fields for global update
#[tracing::instrument(skip(state, auth_user, payload), name = "globals::validate_global")]
pub async fn validate_global(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(payload): Json<ValidateRequest>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return validation_error_response_simple("Global not found"),
    };

    match check_access_or_forbid(&state, def.access.update.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => return validation_error_response_simple("Access denied"),
        Err(_) => return validation_error_response_simple("Access check failed"),
        _ => {}
    }

    let mut form_data = values_to_string_map(&payload.data);

    // Field write access stripping is now handled inside service::validate_document
    // via WriteHooks::field_write_denied.

    transform_select_has_many(&mut form_data, &def.fields);
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let is_draft = payload.draft && def.has_drafts();
    let locale_ctx =
        LocaleContext::from_locale_string(payload.locale.as_deref(), &state.config.locale);

    let gtable = global_table(&slug);
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc = get_user_doc(&auth_user).cloned();

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
            form_data: &form_data,
            join_data: &join_data,
            is_draft,
            soft_delete: false,
            locale_ctx: locale_ctx.as_ref(),
            user_doc: user_doc.as_ref(),
        })
    })
    .await;

    handle_validation_result(result, &auth_user, &state)
}
