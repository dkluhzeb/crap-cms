use std::collections::HashMap;

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
                paths, redirect_response, split_sidebar_fields, translate_validation_errors,
            },
        },
    },
    config::LocaleConfig,
    core::{
        AuthUser, Document, GlobalDefinition, ReqContext, SharedCache, SharedEventTransport,
        ValidationError,
    },
    db::{DbPool, LocaleContext, LocaleMode},
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError},
};

/// Parameters for the blocking global-update task.
struct UpdateParams {
    pool: DbPool,
    runner: HookRunner,
    event_transport: Option<SharedEventTransport>,
    cache: Option<SharedCache>,
    slug: String,
    def: GlobalDefinition,
    form: FormData,
    locale_ctx: Option<LocaleContext>,
    locale: Option<String>,
    locale_config: LocaleConfig,
    draft: bool,
    user_doc: Option<Document>,
    ui_locale: Option<String>,
    action: String,
}

/// Execute the global update (or unpublish) inside a blocking task.
fn update_global_document_blocking(
    params: UpdateParams,
) -> Result<(Document, ReqContext), ServiceError> {
    let ctx = ServiceContext::global(&params.slug, &params.def)
        .pool(&params.pool)
        .runner(&params.runner)
        .user(params.user_doc.as_ref())
        .event_transport(params.event_transport)
        .cache(params.cache)
        .locale_config(Some(&params.locale_config))
        .build();

    if params.action == "unpublish" && params.def.has_versions() {
        let doc = service::unpublish_global_document(&ctx)?;

        Ok((doc, ReqContext::new()))
    } else {
        service::update_global_document(
            &ctx,
            service::WriteInput::builder(params.form)
                .locale_ctx(params.locale_ctx.as_ref())
                .locale(params.locale)
                .draft(params.draft)
                .ui_locale(params.ui_locale)
                .build(),
        )
    }
}

/// Build the validation error response with re-rendered form fields.
fn render_validation_error(
    state: &AdminState,
    def: &GlobalDefinition,
    form: &FormData,
    ve: &ValidationError,
    auth_user: &Option<Extension<AuthUser>>,
) -> Response {
    let locale = auth_user
        .as_ref()
        .map(|Extension(au)| au.ui_locale.as_str())
        .unwrap_or("en");

    let error_map = translate_validation_errors(ve, &state.translations, locale);
    let toast_msg = state.translations.get(locale, "validation.error_summary");

    let mut fields = build_field_contexts(&def.fields, form.raw(), &error_map, false, false);

    let doc_fields = form.to_doc_fields();

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &doc_fields,
        state,
        &EnrichOptions::builder(&error_map).build(),
    );

    let form_data_json = json!(doc_fields);
    apply_display_conditions(
        &mut fields,
        &def.fields,
        &form_data_json,
        &state.hook_runner,
        false,
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
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response(paths::DASHBOARD),
    };

    // Field write access is now checked inside service::update_global_in_conn.

    let mut form = FormData::from_raw(form_data, &def.fields);
    let action = form.take_action();
    let locale_ctx =
        LocaleContext::from_locale_string(form.take_locale().as_deref(), &state.config.locale)
            .unwrap_or(None);

    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });

    let form_for_error = form.clone();

    let params = UpdateParams {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        event_transport: state.event_transport.clone(),
        cache: state.cache.clone(),
        slug: slug.clone(),
        def: def.clone(),
        form,
        locale_ctx,
        locale,
        locale_config: state.config.locale.clone(),
        draft: action == "save_draft",
        user_doc: get_user_doc(&auth_user).cloned(),
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
                render_validation_error(&state, &def, &form_for_error, ve, &auth_user)
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
