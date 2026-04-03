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
                check_access_or_forbid, forbidden, get_event_user, get_user_doc,
                htmx_redirect_with_created, redirect_response, strip_write_denied_string_fields,
                toast_only_error,
            },
        },
    },
    core::{
        CollectionDefinition, Document,
        auth::AuthUser,
        event::{EventOperation, EventTarget},
        upload,
        validate::ValidationError,
    },
    db::query::{AccessResult, LocaleContext, LocaleMode},
    hooks::lifecycle::PublishEventInput,
    service,
};

/// Handle post-create success: commit upload, enqueue conversions, publish event, send verification email.
fn handle_create_success(
    state: &AdminState,
    def: &CollectionDefinition,
    slug: &str,
    doc: &Document,
    upload_result: Option<UploadResult>,
    auth_user: &Option<Extension<AuthUser>>,
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

    state.hook_runner.publish_event(
        &state.event_bus,
        &def.hooks,
        def.live.as_ref(),
        PublishEventInput::builder(EventTarget::Collection, EventOperation::Create)
            .collection(slug.to_string())
            .document_id(doc.id.clone())
            .data(doc.fields.clone())
            .edited_by(get_event_user(auth_user))
            .build(),
    );

    if def.is_auth_collection()
        && def.auth.as_ref().is_some_and(|a| a.verify_email)
        && let Some(user_email) = doc.fields.get("email").and_then(|v| v.as_str())
    {
        service::send_verification_email(
            state.pool.clone(),
            state.config.email.clone(),
            state.email_renderer.clone(),
            state.config.server.clone(),
            slug.to_string(),
            doc.id.to_string(),
            user_email.to_string(),
        );
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
) -> Result<Result<service::WriteResult, anyhow::Error>, task::JoinError> {
    let pool = state.pool.clone();
    let runner = state.hook_runner.clone();
    let slug_owned = slug.to_string();
    let def_owned = def.clone();
    let user_doc = get_user_doc(auth_user).cloned();
    let locale = input.locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });
    let ui_locale = auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone());

    task::spawn_blocking(move || {
        service::create_document(
            &pool,
            &runner,
            &slug_owned,
            &def_owned,
            service::WriteInput::builder(input.form_data, &input.join_data)
                .password(input.password.as_deref())
                .locale_ctx(input.locale_ctx.as_ref())
                .locale(locale)
                .draft(input.draft)
                .ui_locale(ui_locale)
                .build(),
            user_doc.as_ref(),
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

    match check_access_or_forbid(&state, def.access.create.as_deref(), &auth_user, None, None) {
        Ok(AccessResult::Denied) => {
            return forbidden(
                &state,
                "You don't have permission to create items in this collection",
            );
        }
        Err(resp) => return *resp,
        _ => {}
    }

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

    if let Err(resp) =
        strip_write_denied_string_fields(&state, &auth_user, &def.fields, "create", &mut form_data)
    {
        return *resp;
    }

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
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale);

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
            handle_create_success(&state, &def, &slug, &doc, upload_result, &auth_user);

            let label = def
                .title_field()
                .and_then(|f| doc.fields.get(f))
                .and_then(|v| v.as_str())
                .unwrap_or(&doc.id);

            htmx_redirect_with_created(&format!("/admin/collections/{}", slug), &doc.id, label)
        }
        Ok(Err(e)) => {
            if let Some(ve) = e.downcast_ref::<ValidationError>() {
                render_form_validation_errors(
                    &state,
                    &def,
                    None,
                    &form_data_clone,
                    &join_data_clone,
                    ve,
                    &auth_user,
                )
            } else {
                error!("Create error: {}", e);
                redirect_response(&format!("/admin/collections/{}/create", slug))
            }
        }
        Err(e) => {
            error!("Create task error: {}", e);
            redirect_response(&format!("/admin/collections/{}/create", slug))
        }
    }
}
