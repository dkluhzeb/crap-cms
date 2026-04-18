use std::collections::HashMap;

use axum::{
    Extension,
    extract::{Path, Request, State},
    response::Response,
};
use serde_json::Value;
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        handlers::{
            collections::shared::{
                UploadParams, UploadResult, process_collection_upload,
                render_form_validation_errors,
            },
            forms::{extract_join_data_from_form, parse_form, transform_select_has_many},
            shared::{
                forbidden, get_user_doc, htmx_redirect_with_created, redirect_response,
                toast_only_error,
            },
        },
    },
    core::{CollectionDefinition, Document, auth::AuthUser, upload},
    db::query::{LocaleContext, LocaleMode},
    service::{self, EmailContext, ServiceError},
};

/// Handle post-create success: commit upload and enqueue conversions.
fn handle_create_success(
    state: &AdminState,
    slug: &str,
    doc: &Document,
    upload_result: Option<UploadResult>,
) {
    if let Some(mut ur) = upload_result {
        ur.guard.commit();

        if !ur.queued_conversions.is_empty()
            && let Ok(conn) = state.pool.get()
            && let Err(e) =
                upload::enqueue_conversions(&conn, slug, &doc.id, &ur.queued_conversions)
        {
            warn!("Failed to enqueue image conversions: {}", e);
        }
    }
}

/// Extract and validate the password field for auth collections.
/// Returns `Ok(None)` for non-auth collections.
fn extract_and_validate_password(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &mut HashMap<String, String>,
) -> Result<Option<String>, Box<Response>> {
    if !def.is_auth_collection() {
        return Ok(None);
    }

    let password = form_data.remove("password");

    if password.as_deref().unwrap_or("").is_empty() {
        return Err(Box::new(toast_only_error("Password is required")));
    }

    if let Some(ref pw) = password
        && let Err(e) = state.config.auth.password_policy.validate(pw)
    {
        return Err(Box::new(toast_only_error(&e.to_string())));
    }

    Ok(password)
}

/// Prepared form data for creating a document.
struct CreateInput {
    form_data: HashMap<String, String>,
    join_data: HashMap<String, Value>,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
}

/// Clone state and run `service::create_document` in a blocking task.
async fn spawn_create(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    auth_user: &Option<Extension<AuthUser>>,
    input: CreateInput,
) -> Result<Result<service::WriteResult, ServiceError>, task::JoinError> {
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let event_transport = state.event_transport.clone();
    let cache = state.cache.clone();
    let email_ctx = Some(EmailContext {
        email_config: state.config.email.clone(),
        email_renderer: state.email_renderer.clone(),
        server_config: state.config.server.clone(),
    });
    let slug_owned = slug.to_string();
    let def_owned = def.clone();
    let user_doc = get_user_doc(auth_user).cloned();
    let locale = input.locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());

    task::spawn_blocking(move || {
        let ctx = service::ServiceContext::collection(&slug_owned, &def_owned)
            .pool(&pool)
            .runner(&runner)
            .user(user_doc.as_ref())
            .event_transport(event_transport)
            .cache(cache)
            .email_ctx(email_ctx)
            .build();

        service::create_document(
            &ctx,
            service::WriteInput::builder(input.form_data, &input.join_data)
                .password(input.password.as_deref())
                .locale_ctx(input.locale_ctx.as_ref())
                .locale(locale)
                .draft(input.draft)
                .ui_locale(ui_locale)
                .build(),
        )
    })
    .await
}

/// POST /admin/collections/{slug} — create a new item
pub async fn create_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    request: Request,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin/collections"),
    };

    // Collection-level access check is handled inside service::create_document_core.

    let (mut form_data, file) = match parse_form(request, &state, &def).await {
        Ok(result) => result,
        Err(e) => {
            error!("{}", e);
            return redirect_response(&format!("/admin/collections/{}/create", slug));
        }
    };

    // Process upload if file present
    let mut upload_result = None;

    if let Some(f) = file
        && def.upload.is_some()
    {
        match process_collection_upload(
            &UploadParams {
                state: &state,
                def: &def,
                slug: &slug,
                doc_id: None,
                locale_ctx: None,
                auth_user: &auth_user,
            },
            &mut form_data,
            f,
        )
        .await
        {
            Ok(ur) => upload_result = Some(ur),
            Err(resp) => return resp,
        }
    }

    // Field write access is now checked inside service::create_document_core.

    let password = match extract_and_validate_password(&state, &def, &mut form_data) {
        Ok(pw) => pw,
        Err(resp) => return *resp,
    };

    transform_select_has_many(&mut form_data, &def.fields);
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let action = form_data.remove("_action").unwrap_or_default();
    let draft = action == "save_draft";

    let form_locale = form_data.remove("_locale");
    let locale_ctx =
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale)
            .unwrap_or(None);

    let form_data_clone = form_data.clone();
    let join_data_clone = join_data.clone();

    let result = spawn_create(
        &state,
        &slug,
        &def,
        &auth_user,
        CreateInput {
            form_data,
            join_data,
            password,
            locale_ctx,
            draft,
        },
    )
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
            handle_create_success(&state, &slug, &doc, upload_result);

            let label = def
                .title_field()
                .and_then(|f| doc.fields.get(f))
                .and_then(|v| v.as_str())
                .unwrap_or(&doc.id);

            htmx_redirect_with_created(&format!("/admin/collections/{}", slug), &doc.id, label)
        }
        Ok(Err(e)) => match e {
            ServiceError::AccessDenied(_) => forbidden(
                &state,
                "You don't have permission to create items in this collection",
            ),
            ServiceError::Validation(ref ve) => render_form_validation_errors(
                &state,
                &def,
                None,
                &form_data_clone,
                &join_data_clone,
                ve,
                &auth_user,
            ),
            other => {
                error!("Create error: {}", other);
                redirect_response(&format!("/admin/collections/{}/create", slug))
            }
        },
        Err(e) => {
            error!("Create task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/create", slug))
        }
    }
}
