use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, Request, State},
    response::Response,
};
use tokio::task;
use tracing::{error, warn};

use crate::{
    admin::{
        AdminState,
        handlers::{
            collections::shared::{
                UploadParams, UploadResult, WriteErrorParams, handle_collection_write_error,
                process_collection_upload,
            },
            forms::{FormData, parse_form},
            shared::{
                get_user_doc, htmx_inline_created, htmx_redirect_with_created,
                parse_request_locale, paths, redirect_response, toast_only_error,
            },
        },
    },
    core::{AuthUser, CollectionDefinition, Document, upload},
    db::LocaleContext,
    service::{
        self, AppInfra, ServiceError,
        op::{Create, CreateArgs, Operation},
    },
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
            && let Ok(conn) = state.infra.pool.get()
            && let Err(e) = upload::enqueue_conversions(
                &conn,
                slug,
                &doc.id,
                &ur.queued_conversions,
                state.config.jobs.system_image_max_attempts(),
            )
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
    form: FormData,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    /// A file was processed and server-derived metadata injected into `form`.
    trusted_upload: bool,
}

/// Owned bundle for the spawn-blocking create body. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct CreateBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    ui_locale: Option<String>,
    input: CreateInput,
}

/// Synchronous body of [`spawn_create`]. Builds the service context and runs
/// the shared [`Create`] operation body with the merged form + join-table
/// data.
fn create_document_blocking(
    args: CreateBlockingInput,
) -> Result<service::WriteResult, ServiceError> {
    let ctx = service::ServiceContext::collection(&args.slug, &args.def)
        .infra(&args.infra)
        .user(args.user_doc.as_ref())
        .ui_locale(args.ui_locale)
        .build();

    let op_args = CreateArgs::builder(args.input.form.into())
        .password(args.input.password)
        .locale_ctx(args.input.locale_ctx)
        .draft(args.input.draft)
        .trusted_upload_metadata(args.input.trusted_upload)
        .build();

    Create::run(&ctx, op_args)
}

/// Clone state and run `service::create_document` in a blocking task.
async fn spawn_create(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    input: CreateInput,
) -> Result<Result<service::WriteResult, ServiceError>, task::JoinError> {
    let ui_locale = auth_user.map(|Extension(au)| au.ui_locale.clone());

    let args = CreateBlockingInput {
        infra: state.infra.clone(),
        slug: slug.to_string(),
        def: def.clone(),
        user_doc: get_user_doc(auth_user).cloned(),
        ui_locale,
        input,
    };

    task::spawn_blocking(move || create_document_blocking(args)).await
}

/// POST /admin/collections/{slug} — create a new item
pub async fn create_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    request: Request,
) -> Response {
    let Some(def) = state.infra.registry.get_collection(&slug).cloned() else {
        return redirect_response(paths::COLLECTIONS_ROOT);
    };

    // Inline-create requests come from `<crap-create-panel>`, which sets
    // `X-Inline-Create: 1` on the form submit. The success response shape
    // differs: no `HX-Redirect` (the panel keeps the parent page),
    // just `X-Created-Id` / `X-Created-Label` headers for the panel's
    // afterRequest listener to fire its `onCreated` callback. Read here
    // because `parse_form` consumes the request below.
    let inline_create = request
        .headers()
        .get("X-Inline-Create")
        .is_some_and(|v| v == "1");

    // Collection-level access check is handled inside service::create_document_in_conn.

    let (form_data, file) = match parse_form(request, &state, &def).await {
        Ok(result) => result,
        Err(e) => {
            error!("{}", e);
            return redirect_response(&paths::collection_create(&slug));
        }
    };

    let mut form = FormData::from_raw(form_data, &def.fields);

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
                auth_user: auth_user.as_ref(),
            },
            form.raw_mut(),
            f,
        )
        .await
        {
            Ok(ur) => upload_result = Some(ur),
            Err(resp) => return resp,
        }
    }

    // Field write access is now checked inside service::create_document_in_conn.

    let password = match extract_and_validate_password(&state, &def, form.raw_mut()) {
        Ok(pw) => pw,
        Err(resp) => return *resp,
    };

    let draft = form.take_action() == "save_draft";
    let locale_ctx = match parse_request_locale(form.take_locale().as_deref(), &state.config.locale)
    {
        Ok(ctx) => ctx,
        Err(msg) => return toast_only_error(&msg),
    };

    let form_for_error = form.clone();

    let result = spawn_create(
        &state,
        &slug,
        &def,
        auth_user.as_ref(),
        CreateInput {
            form,
            password,
            locale_ctx,
            draft,
            trusted_upload: upload_result.is_some(),
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

            if inline_create {
                htmx_inline_created(&doc.id, label)
            } else {
                htmx_redirect_with_created(&paths::collection(&slug), &doc.id, label)
            }
        }
        Ok(Err(e)) => {
            handle_collection_write_error(WriteErrorParams {
                state: &state,
                def: &def,
                form: &form_for_error,
                err: e,
                doc_id: None,
                auth_user: auth_user.as_ref(),
            })
            .await
        }
        Err(e) => {
            error!("Create task error: {}", e);
            redirect_response(&paths::collection_create(&slug))
        }
    }
}
