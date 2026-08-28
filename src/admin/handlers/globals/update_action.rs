use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{Form, Path, State},
    response::Response,
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, GlobalContext, GlobalPermissions, PageMeta, PageType,
            page::globals::GlobalFormErrorPage,
        },
        handlers::{
            forms::FormData,
            shared::{
                EnrichOptions, apply_display_conditions, build_field_contexts,
                enrich_field_contexts, forbidden, get_user_doc, htmx_redirect, page_with_toast,
                parse_request_locale, paths, redirect_response, split_sidebar_fields,
                toast_only_error, translate_validation_errors,
            },
        },
    },
    core::{AuthUser, Document, GlobalDefinition, ReqContext, ValidationError},
    db::LocaleContext,
    hooks::ConditionContext,
    service::{
        AppInfra, ServiceContext, ServiceError,
        op::{Operation, UnpublishGlobal, UnpublishGlobalArgs, UpdateGlobal, UpdateGlobalArgs},
    },
};

/// Parameters for the blocking global-update task. Process-stable dependencies
/// come from the shared [`AppInfra`]; the rest is per-call.
struct UpdateParams {
    infra: Arc<AppInfra>,
    slug: String,
    def: GlobalDefinition,
    form: FormData,
    locale_ctx: Option<LocaleContext>,
    draft: bool,
    user_doc: Option<Document>,
    ui_locale: Option<String>,
    action: String,
}

/// Execute the global update (or unpublish) inside a blocking task via the
/// shared operation bodies.
fn update_global_document_blocking(
    params: UpdateParams,
) -> Result<(Document, ReqContext), ServiceError> {
    let ctx = ServiceContext::global(&params.slug, &params.def)
        .infra(&params.infra)
        .user(params.user_doc.as_ref())
        .ui_locale(params.ui_locale)
        .build();

    // Route on the action alone — the capability gate (versioning required)
    // lives in the shared operation body. Guarding here used to silently fall
    // through to a full update on a non-versioned global, publishing the form
    // data instead of erroring.
    if params.action == "unpublish" {
        let doc = UnpublishGlobal::run(&ctx, UnpublishGlobalArgs::default())?;

        Ok((doc, ReqContext::new()))
    } else {
        let args = UpdateGlobalArgs::builder(params.form.into())
            .locale_ctx(params.locale_ctx)
            .draft(params.draft)
            .build();

        UpdateGlobal::run(&ctx, args)
    }
}

/// Build the validation error response with re-rendered form fields.
fn render_validation_error(
    state: &AdminState,
    def: &GlobalDefinition,
    form: &FormData,
    ve: &ValidationError,
    auth_user: Option<&Extension<AuthUser>>,
) -> Response {
    let locale = auth_user.map_or("en", |Extension(au)| au.ui_locale.as_str());

    let error_map = translate_validation_errors(ve, &state.translations, locale);
    let toast_msg = state.translations.get(locale, "validation.error_summary");

    let mut fields = build_field_contexts(&def.fields, form.raw(), &error_map, false, false);

    let doc_fields = form.to_doc_fields();

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &doc_fields,
        state,
        &EnrichOptions::builder(&error_map)
            .user(get_user_doc(auth_user))
            .build(),
    );

    let form_data_json = json!(doc_fields);
    let cond_ctx = ConditionContext {
        collection: &def.slug,
        operation: "update",
        user: get_user_doc(auth_user),
        ui_locale: auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: None,
        options: None,
    };
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &form_data_json,
        &state.infra.hook_runner,
        false,
        &cond_ctx,
    );

    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let base = BasePageContext::for_handler(
        state,
        None,
        auth_user,
        PageMeta::new(PageType::GlobalEdit, def.display_name()),
    );

    let perms = GlobalPermissions::for_user(state, def, auth_user);

    let ctx = GlobalFormErrorPage {
        base,
        global: GlobalContext::from_def(def),
        perms,
        fields: main_fields,
        sidebar_fields,
    };

    page_with_toast(state, "globals/edit", &ctx, toast_msg)
}

/// POST /admin/globals/{slug} — update a global
pub async fn update_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(form_data): Form<HashMap<String, String>>,
) -> Response {
    let def = match state.infra.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response(paths::DASHBOARD),
    };

    // Field write access is now checked inside service::update_global_in_conn.

    let mut form = FormData::from_raw(form_data, &def.fields);
    let action = form.take_action();
    let locale_ctx = match parse_request_locale(form.take_locale().as_deref(), &state.config.locale)
    {
        Ok(ctx) => ctx,
        Err(msg) => return toast_only_error(&msg),
    };

    let form_for_error = form.clone();

    let params = UpdateParams {
        infra: state.infra.clone(),
        slug: slug.clone(),
        def: def.clone(),
        form,
        locale_ctx,
        draft: action == "save_draft",
        user_doc: get_user_doc(auth_user.as_ref()).cloned(),
        ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
        action,
    };

    let result = task::spawn_blocking(move || update_global_document_blocking(params)).await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&paths::global(&slug)),
        Ok(Err(e)) => match e {
            ServiceError::AccessDenied(_) => {
                forbidden(&state, "You don't have permission to update this global")
            }
            ServiceError::Validation(ref ve) => {
                render_validation_error(&state, &def, &form_for_error, ve, auth_user.as_ref())
            }
            other => {
                error!("Global update error: {}", other);
                redirect_response(&paths::global(&slug))
            }
        },
        Err(e) => {
            error!("Global update task error: {}", e);
            redirect_response(&paths::global(&slug))
        }
    }
}
