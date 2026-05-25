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
            shared::{check_access_or_forbid, get_user_doc},
            validate::{
                RunValidationParams, ValidateRequest, handle_validation_result, run_validation,
                validation_error_response_simple,
            },
        },
    },
    core::auth::AuthUser,
    db::{AccessResult, query::LocaleContext},
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
    let Some(def) = state.registry.get_collection(&slug).cloned() else {
        return validation_error_response_simple("Collection not found");
    };

    match check_access_or_forbid(
        &state,
        def.access.update.as_deref(),
        auth_user.as_ref(),
        None,
        None,
    ) {
        Ok(AccessResult::Denied) => return validation_error_response_simple("Access denied"),
        Err(_) => return validation_error_response_simple("Access check failed"),
        _ => {}
    }

    let data = prepare_form_for_validation(&state, &def, auth_user.as_ref(), &payload, "update");

    let is_draft = payload.draft && def.has_drafts();
    let locale_ctx =
        LocaleContext::from_locale_string(payload.locale.as_deref(), &state.config.locale)
            .unwrap_or(None);
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let user_doc = get_user_doc(auth_user.as_ref()).cloned();

    let result = task::spawn_blocking(move || {
        run_validation(&RunValidationParams {
            pool: &pool,
            runner: &runner,
            hooks: &def_owned.hooks,
            fields: &def_owned.fields,
            slug: &slug_owned,
            table_name: &slug_owned,
            operation: "update",
            exclude_id: Some(&id_owned),
            data: &data,
            is_draft,
            soft_delete: def_owned.soft_delete,
            locale_ctx: locale_ctx.as_ref(),
            user_doc: user_doc.as_ref(),
        })
    })
    .await;

    handle_validation_result(result, auth_user.as_ref(), &state)
}
