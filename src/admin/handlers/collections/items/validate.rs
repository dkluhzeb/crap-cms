//! Validation-only endpoints for collection items.
//!
//! These run the full before_validate → validate pipeline inside a rolled-back transaction,
//! returning JSON `{ valid: true }` or `{ valid: false, errors: { ... } }`.
//! Used by the `<crap-validate-form>` component to validate fields before uploading files.

use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde_json::json;
use tokio::task;

use crate::{
    admin::{
        AdminState,
        handlers::{
            collections::forms::{extract_join_data_from_form, transform_select_has_many},
            shared::{get_user_doc, strip_write_denied_string_fields},
            validate::{
                ValidateRequest, validation_error_response, validation_ok_response,
                values_to_string_map,
            },
        },
    },
    core::{auth::AuthUser, validate::ValidationError},
    db::query::LocaleContext,
    hooks::{HookContext, ValidationCtx},
    service,
};

/// POST /admin/collections/{slug}/validate — validate fields for create
#[tracing::instrument(skip(state, auth_user, payload), name = "collections::validate_create")]
pub async fn validate_create(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Json(payload): Json<ValidateRequest>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return validation_error_response_simple("Collection not found"),
    };

    let mut form_data = values_to_string_map(&payload.data);

    // Strip field-level create-denied fields
    if let Err(_resp) =
        strip_write_denied_string_fields(&state, &auth_user, &def.fields, "create", &mut form_data)
    {
        return validation_error_response_simple("Access check failed");
    }

    // Remove password — not relevant for validation
    form_data.remove("password");

    transform_select_has_many(&mut form_data, &def.fields);
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let is_draft = payload.draft && def.has_drafts();
    let locale_ctx =
        LocaleContext::from_locale_string(payload.locale.as_deref(), &state.config.locale);

    // For upload collections, pre-populate system-managed upload metadata fields
    // with placeholders so they don't fail required checks. These are normally set
    // by upload::inject_upload_metadata() which hasn't run yet (file not uploaded).
    // Uses an explicit whitelist so user-defined hidden/readonly fields are unaffected.
    if let Some(upload_config) = &def.upload {
        let system_fields = upload_config.system_field_names();
        for name in system_fields {
            form_data.insert(name, "_pending_upload".to_string());
        }
    }

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let def_owned = def.clone();
    let user_doc = get_user_doc(&auth_user).cloned();

    let result = task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        let tx = conn.transaction()?;

        let hook_data = service::build_hook_data(&form_data, &join_data);
        let hook_ctx = HookContext::builder(&slug_owned, "create")
            .data(hook_data)
            .locale(locale_ctx.as_ref().and_then(|ctx| {
                if let crate::db::query::LocaleMode::Single(l) = &ctx.mode {
                    Some(l.clone())
                } else {
                    None
                }
            }))
            .draft(is_draft)
            .user(user_doc.as_ref())
            .build();
        let val_ctx = ValidationCtx::builder(&tx, &slug_owned)
            .draft(is_draft)
            .locale_ctx(locale_ctx.as_ref())
            .build();

        let result =
            runner.run_before_write(&def_owned.hooks, &def_owned.fields, hook_ctx, &val_ctx);

        // Always rollback — this is validation only
        drop(tx);

        result
    })
    .await;

    match result {
        Ok(Ok(_)) => validation_ok_response(),
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let locale = auth_user
                    .as_ref()
                    .map(|Extension(au)| au.ui_locale.as_str())
                    .unwrap_or("en");
                validation_error_response(ve, &state.translations, locale)
            } else {
                validation_error_response_simple(&format!("Validation error: {}", e))
            }
        }
        Err(e) => validation_error_response_simple(&format!("Internal error: {}", e)),
    }
}

/// POST /admin/collections/{slug}/{id}/validate — validate fields for update
#[tracing::instrument(skip(state, auth_user, payload), name = "collections::validate_update")]
pub async fn validate_update(
    State(state): State<AdminState>,
    Path((slug, id)): Path<(String, String)>,
    auth_user: Option<Extension<AuthUser>>,
    Json(payload): Json<ValidateRequest>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return validation_error_response_simple("Collection not found"),
    };

    let mut form_data = values_to_string_map(&payload.data);

    // Strip field-level update-denied fields
    if let Err(_resp) =
        strip_write_denied_string_fields(&state, &auth_user, &def.fields, "update", &mut form_data)
    {
        return validation_error_response_simple("Access check failed");
    }

    // Remove password — not relevant for validation
    form_data.remove("password");

    transform_select_has_many(&mut form_data, &def.fields);
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let is_draft = payload.draft && def.has_drafts();
    let locale_ctx =
        LocaleContext::from_locale_string(payload.locale.as_deref(), &state.config.locale);

    // For upload collections, pre-populate system-managed upload metadata fields
    if let Some(upload_config) = &def.upload {
        let system_fields = upload_config.system_field_names();
        for name in system_fields {
            form_data.insert(name, "_pending_upload".to_string());
        }
    }

    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.clone();
    let id_owned = id.clone();
    let def_owned = def.clone();
    let user_doc = get_user_doc(&auth_user).cloned();

    let result = task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        let tx = conn.transaction()?;

        let hook_data = service::build_hook_data(&form_data, &join_data);
        let hook_ctx = HookContext::builder(&slug_owned, "update")
            .data(hook_data)
            .locale(locale_ctx.as_ref().and_then(|ctx| {
                if let crate::db::query::LocaleMode::Single(l) = &ctx.mode {
                    Some(l.clone())
                } else {
                    None
                }
            }))
            .draft(is_draft)
            .user(user_doc.as_ref())
            .build();
        let val_ctx = ValidationCtx::builder(&tx, &slug_owned)
            .exclude_id(Some(&id_owned))
            .draft(is_draft)
            .locale_ctx(locale_ctx.as_ref())
            .build();

        let result =
            runner.run_before_write(&def_owned.hooks, &def_owned.fields, hook_ctx, &val_ctx);

        // Always rollback — this is validation only
        drop(tx);

        result
    })
    .await;

    match result {
        Ok(Ok(_)) => validation_ok_response(),
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                let locale = auth_user
                    .as_ref()
                    .map(|Extension(au)| au.ui_locale.as_str())
                    .unwrap_or("en");
                validation_error_response(ve, &state.translations, locale)
            } else {
                validation_error_response_simple(&format!("Validation error: {}", e))
            }
        }
        Err(e) => validation_error_response_simple(&format!("Internal error: {}", e)),
    }
}

/// Quick error response for non-validation failures.
fn validation_error_response_simple(msg: &str) -> Response {
    Json(json!({
        "valid": false,
        "errors": { "_form": msg },
    }))
    .into_response()
}
