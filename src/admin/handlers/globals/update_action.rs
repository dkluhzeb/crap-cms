use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{Form, Path, State},
    response::Response,
};
use serde_json::json;
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
                EnrichOptions, HxNav, PageRequest, apply_display_conditions, build_field_contexts,
                editor_read_ctx, enrich_field_contexts, forbidden, get_user_doc, htmx_redirect,
                is_non_default_locale, page_with_toast, parse_request_locale, paths,
                redirect_response, split_sidebar_fields, strip_locale_locked_form_fields,
                toast_only_error, translate_validation_errors, write_error_toast,
            },
        },
    },
    core::{
        AuthUser, Document, GlobalDefinition, ReqContext, ValidationError,
        spawn_blocking_in_label_locale,
    },
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
    def: Arc<GlobalDefinition>,
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
        // The form submits shared (locale-locked) fields read-only under a
        // non-default locale; strip them exactly as the collection publish
        // path does, so the service's shared-field guard doesn't reject the
        // translation save.
        let data = strip_locale_locked_form_fields(
            params.form.into(),
            &params.def.fields,
            params.locale_ctx.as_ref(),
        );
        let args = UpdateGlobalArgs::builder(data)
            .locale_ctx(params.locale_ctx)
            .draft(params.draft)
            .build();

        UpdateGlobal::run(&ctx, args)
    }
}

/// What the validation re-render needs beyond the errors themselves.
struct ValidationRender<'a> {
    state: &'a AdminState,
    def: &'a GlobalDefinition,
    form: &'a FormData,
    auth_user: Option<&'a Extension<AuthUser>>,
    /// The content locale the form was submitted in (`_locale`), taken out of
    /// the raw form before the write — the re-render has to put it back, or
    /// the corrected save writes the translation into the default locale.
    submitted_locale: Option<&'a str>,
    /// How the submit was issued — an htmx form post targeting `#main` gets
    /// the fragment back, not a second full document.
    hx: HxNav,
}

/// Build the validation error response with re-rendered form fields.
async fn render_validation_error(p: &ValidationRender<'_>, ve: &ValidationError) -> Response {
    let locale = p
        .auth_user
        .map_or("en", |Extension(au)| au.ui_locale.as_str());

    let error_map = translate_validation_errors(ve, &p.state.translations, locale);
    let toast_msg = p.state.translations.get(locale, "validation.error_summary");

    // Same locale resolution the success-path form makes, so the re-render
    // stays in the submitted locale: the read context for relationship labels,
    // and the lock on the shared fields. The picker itself comes from the same
    // locale via `with_editor_locale` below.
    let locale_ctx = editor_read_ctx(p.state, p.submitted_locale);
    let non_default_locale = is_non_default_locale(p.state, p.submitted_locale);

    let mut fields = build_field_contexts(
        &p.def.fields,
        p.form.raw(),
        &error_map,
        true,
        non_default_locale,
    );

    let doc_fields = p.form.to_doc_fields();

    enrich_field_contexts(
        &mut fields,
        &p.def.fields,
        &doc_fields,
        p.state,
        &EnrichOptions::builder(&error_map)
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .user(get_user_doc(p.auth_user))
            .locale_ctx(locale_ctx.as_ref())
            .build(),
    );

    let form_data_json = json!(doc_fields);
    let cond_ctx = ConditionContext {
        collection: &p.def.slug,
        operation: "update",
        user: get_user_doc(p.auth_user),
        ui_locale: p.auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: p.submitted_locale,
        options: None,
    };
    apply_display_conditions(
        &mut fields,
        &p.def.fields,
        &form_data_json,
        &p.state.infra.hook_runner,
        true,
        &cond_ctx,
    );

    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let base = BasePageContext::for_handler(
        p.state,
        None,
        p.auth_user,
        PageMeta::new(PageType::GlobalEdit, p.def.display_name()),
    )
    .with_editor_locale(p.submitted_locale, p.state);

    let perms = GlobalPermissions::for_user(p.state, p.def, p.auth_user);

    let ctx = GlobalFormErrorPage {
        base,
        global: GlobalContext::from_def(p.def),
        perms,
        fields: main_fields,
        sidebar_fields,
        unsaved: true,
    };

    page_with_toast(
        p.state,
        PageRequest::new(p.hx, p.auth_user),
        "globals/edit",
        &ctx,
        toast_msg,
    )
    .await
}

/// POST /admin/globals/{slug} — update a global
pub async fn update_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    hx: HxNav,
    Form(form_data): Form<HashMap<String, String>>,
) -> Response {
    let def = match state.infra.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response(paths::DASHBOARD),
    };

    // Field write access is now checked inside service::update_global_in_conn.

    let mut form = FormData::from_raw(form_data, &def.fields);
    let action = form.take_action();

    // Kept past the write for the error re-render — see `ValidationRender`.
    let submitted_locale = form.take_locale();
    let locale_ctx = match parse_request_locale(submitted_locale.as_deref(), &state.config.locale) {
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

    let result =
        spawn_blocking_in_label_locale(move || update_global_document_blocking(params)).await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&paths::global(&slug)),
        Ok(Err(e)) => match e {
            ServiceError::AccessDenied(_) => {
                forbidden(&state, "You don't have permission to update this global")
            }
            ServiceError::Validation(ref ve) => {
                let render = ValidationRender {
                    state: &state,
                    def: &def,
                    form: &form_for_error,
                    auth_user: auth_user.as_ref(),
                    submitted_locale: submitted_locale.as_deref(),
                    hx,
                };

                render_validation_error(&render, ve).await
            }
            // A hook abort, a dangling reference, a lock-retry exhaustion:
            // toast it over the form exactly as the collection edit form does,
            // instead of redirecting back as if the save had gone through.
            other => toast_only_error(&write_error_toast(
                "Global update",
                other,
                state.infra.pool.kind(),
            )),
        },
        Err(e) => {
            error!("Global update task error: {}", e);
            redirect_response(&paths::global(&slug))
        }
    }
}
