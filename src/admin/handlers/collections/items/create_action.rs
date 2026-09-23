use std::sync::Arc;

use axum::{
    Extension,
    extract::{Path, Request, State},
    response::Response,
};
use tokio::task::JoinError;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::{
            collections::shared::{SubmittedMeta, WriteErrorParams, handle_collection_write_error},
            forms::{FormData, parse_form},
            shared::{
                HxNav, get_user_doc, htmx_inline_created, htmx_redirect_with_created,
                parse_request_locale, paths, redirect_response, toast_only_error,
            },
        },
    },
    core::{
        AuthUser, CollectionDefinition, Document, SharedStorage, spawn_blocking_in_label_locale,
        upload::UploadedFile,
    },
    db::LocaleContext,
    service::{
        self, AppInfra, ServiceContext, ServiceError,
        op::{Create, CreateArgs, Operation},
        upload::{CreateUploadInput, create_upload},
    },
};

/// Prepared form data for creating a document.
struct CreateInput {
    form: FormData,
    /// The multipart file, if the submission carried one. Stored by the upload
    /// service inside the blocking task — never out here, where a dropped
    /// handler future would leave the bytes behind a row the blocking task
    /// went on to commit.
    file: Option<UploadedFile>,
    password: Option<String>,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
}

/// Owned bundle for the spawn-blocking create body. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct CreateBlockingInput {
    infra: Arc<AppInfra>,
    slug: String,
    def: CollectionDefinition,
    user_doc: Option<Document>,
    ui_locale: Option<String>,
    max_file_size: u64,
    image_max_attempts: u32,
    input: CreateInput,
}

/// Everything [`run_write`] needs to perform the create itself. All fields are
/// required and it is built in exactly one place.
struct WriteArgs<'a> {
    storage: &'a SharedStorage,
    ui_locale: Option<String>,
    max_file_size: u64,
    image_max_attempts: u32,
    input: CreateInput,
}

/// The create itself: an upload write when the submission carries a file, a
/// plain create otherwise.
///
/// A file goes through `service::upload`, the one entry that owns the file
/// lifecycle (store, metadata, queued conversions) for every surface.
fn run_write(
    ctx: &ServiceContext<'_>,
    args: WriteArgs<'_>,
) -> Result<service::WriteResult, ServiceError> {
    if let Some(file) = args.input.file {
        let result = create_upload(
            ctx,
            CreateUploadInput {
                storage: args.storage,
                file: &file,
                form: args.input.form,
                locale_ctx: args.input.locale_ctx.as_ref(),
                password: args.input.password,
                ui_locale: args.ui_locale,
                draft: args.input.draft,
                upload_max_file_size: args.max_file_size,
                image_max_attempts: args.image_max_attempts,
            },
        )?;

        return Ok((result.doc, result.req_context));
    }

    let op_args = CreateArgs::builder(args.input.form.into())
        .password(args.input.password)
        .locale_ctx(args.input.locale_ctx)
        .draft(args.input.draft)
        .build();

    Create::run(ctx, op_args)
}

/// Synchronous body of [`spawn_create`]. Builds the service context and runs
/// the create.
fn create_document_blocking(
    args: CreateBlockingInput,
) -> Result<service::WriteResult, ServiceError> {
    let ctx = ServiceContext::collection(&args.slug, &args.def)
        .infra(&args.infra)
        .user(args.user_doc.as_ref())
        .ui_locale(args.ui_locale.clone())
        .build();

    run_write(
        &ctx,
        WriteArgs {
            storage: &args.infra.storage,
            ui_locale: args.ui_locale.clone(),
            max_file_size: args.max_file_size,
            image_max_attempts: args.image_max_attempts,
            input: args.input,
        },
    )
}

/// Clone state and run `service::create_document` in a blocking task.
async fn spawn_create(
    state: &AdminState,
    slug: &str,
    def: &CollectionDefinition,
    auth_user: Option<&Extension<AuthUser>>,
    input: CreateInput,
) -> Result<Result<service::WriteResult, ServiceError>, JoinError> {
    let ui_locale = auth_user.map(|Extension(au)| au.ui_locale.clone());

    let args = CreateBlockingInput {
        infra: state.infra.clone(),
        slug: slug.to_string(),
        def: def.clone(),
        user_doc: get_user_doc(auth_user).cloned(),
        ui_locale,
        max_file_size: state.config.upload.max_file_size,
        image_max_attempts: state.config.jobs.system_image_max_attempts(),
        input,
    };

    spawn_blocking_in_label_locale(move || create_document_blocking(args)).await
}

/// POST /admin/collections/{slug} — create a new item
pub async fn create_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    hx: HxNav,
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

    // A file only reaches the write when the collection accepts one.
    let file = file.filter(|_| def.is_upload_collection());

    // Field and collection write access, and the password policy, are checked
    // inside the service write — a violation comes back as a `password` field
    // error the form re-render shows in the viewer's locale.
    let password = form.take_password(&def);

    let draft = form.take_action() == "save_draft";

    // Kept past the write: an error re-render has to put `_locale` back into
    // the form, or the corrected save lands in the default locale.
    let submitted_locale = form.take_locale();
    let locale_ctx = match parse_request_locale(submitted_locale.as_deref(), &state.config.locale) {
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
            file,
            password,
            locale_ctx,
            draft,
        },
    )
    .await;

    match result {
        Ok(Ok((doc, _req_context))) => {
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
                meta: SubmittedMeta::new(submitted_locale.as_deref(), None),
                hx,
            })
            .await
        }
        Err(e) => {
            error!("Create task error: {}", e);
            redirect_response(&paths::collection_create(&slug))
        }
    }
}
