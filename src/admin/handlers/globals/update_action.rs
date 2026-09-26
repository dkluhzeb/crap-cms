use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension,
    extract::{Form, Path, State},
    response::Response,
};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, GlobalContext, GlobalPermissions, PageMeta, PageType,
            page::{RevisionConflictNotice, globals::GlobalFormErrorPage},
        },
        handlers::{
            forms::FormData,
            shared::{
                EnrichOptions, ErrorLabels, HxNav, PageRequest, apply_display_conditions,
                build_field_contexts, conflict_keeps_form, editor_read_ctx, enrich_field_contexts,
                forbidden, form_condition_data, get_user_doc, global_form_fields,
                global_read_denials, htmx_redirect, is_non_default_locale, page_with_toast,
                parse_request_locale, paths, readable_form_fields, redirect_response,
                split_sidebar_fields, strip_locale_locked_form_fields, toast_only_error,
                translate_validation_errors, ui_locale_of, unpublish_conflict_response,
                write_error_response,
            },
        },
    },
    core::{
        AuthUser, Document, GlobalDefinition, ReqContext, ValidationError, spawn_request_blocking,
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
    /// The revision the edit form was loaded at (`_revision`).
    expected_revision: Option<i64>,
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
        let unpublish = UnpublishGlobalArgs::new(true, params.expected_revision);
        let doc = UnpublishGlobal::run(&ctx, unpublish)?;

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
            .expected_revision(params.expected_revision)
            .build();

        UpdateGlobal::run(&ctx, args)
    }
}

/// What an error re-render of the form needs beyond the errors themselves.
struct FormRender<'a> {
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
    /// The revision the re-rendered form submits back (`_revision`): the one
    /// the form was loaded at, or the global's current one after a conflict.
    revision: Option<i64>,
    /// Set when the save was refused because the global changed since the
    /// form was loaded.
    conflict: Option<RevisionConflictNotice>,
}

/// Build the validation error response with re-rendered form fields.
async fn render_validation_error(p: &FormRender<'_>, ve: &ValidationError) -> Response {
    let locale = ui_locale_of(p.auth_user);

    let labels = ErrorLabels::new(&p.def.fields, Some(p.form.raw()));
    let error_map = translate_validation_errors(ve, &labels, &p.state.translations, locale);
    let toast_msg = p.state.translations.get(locale, "validation.error_summary");

    render_form(p, &error_map, toast_msg).await
}

/// Re-render the form after its save was refused by a revision conflict: the
/// editor's unsaved values stay, `p` carries the global's current revision and
/// the conflict notice (reload, or overwrite by saving again).
async fn render_revision_conflict(p: &FormRender<'_>) -> Response {
    let locale = ui_locale_of(p.auth_user);
    let toast_msg = p.state.translations.get(locale, "revision_conflict_title");

    render_form(p, &HashMap::new(), toast_msg).await
}

/// Re-render the submitted form with `error_map` inline and `toast_msg` as
/// the toast.
async fn render_form(
    p: &FormRender<'_>,
    error_map: &HashMap<String, String>,
    toast_msg: &str,
) -> Response {
    // Same locale resolution the success-path form makes, so the re-render
    // stays in the submitted locale: the read context for relationship labels,
    // and the lock on the shared fields. The picker itself comes from the same
    // locale via `with_editor_locale` below.
    let locale_ctx = editor_read_ctx(p.state, p.submitted_locale);
    let non_default_locale = is_non_default_locale(p.state, p.submitted_locale);

    // No input for a field the viewer may not read, exactly as on the edit form
    // — in each row, what the viewer may not read in that row.
    let denied = global_read_denials(p.state, p.def, p.auth_user, p.submitted_locale).await;
    let form_fields = readable_form_fields(&p.def.fields, &denied.flat);

    let mut fields = build_field_contexts(
        &form_fields,
        p.form.raw(),
        error_map,
        true,
        non_default_locale,
    );

    let doc_fields = p.form.to_doc_fields();

    enrich_field_contexts(
        &mut fields,
        &form_fields,
        &doc_fields,
        p.state,
        &EnrichOptions::builder(error_map)
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .user(get_user_doc(p.auth_user))
            .locale_ctx(locale_ctx.as_ref())
            .build(),
    );

    denied.rows.prune(&mut fields);

    let cond_ctx = ConditionContext {
        collection: &p.def.slug,
        operation: "update",
        user: get_user_doc(p.auth_user),
        ui_locale: p.auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: p.submitted_locale,
        options: None,
    };
    // The submitted values, decoded like the write they were meant for — the
    // same data view the edit form conditions on.
    apply_display_conditions(
        &mut fields,
        &form_fields,
        &form_condition_data(&p.def.fields, p.form),
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
        revision: p.revision,
        revision_conflict: p.conflict.clone(),
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

    // Parsed against the fields the edit form rendered for this viewer: an
    // input it never rendered is absent from the write, not an unchecked box.
    let locale = form_data.get("_locale").map(String::as_str);
    let form_fields = global_form_fields(&state, &def, auth_user.as_ref(), locale).await;
    let mut form = FormData::from_raw(form_data, &form_fields);
    let action = form.take_action();

    // Kept past the write for the error re-render — see `FormRender`.
    let submitted_locale = form.take_locale();
    let locale_ctx = match parse_request_locale(submitted_locale.as_deref(), &state.config.locale) {
        Ok(ctx) => ctx,
        Err(msg) => return toast_only_error(&msg),
    };

    // The revision the form was loaded at: the save is refused when someone
    // else saved the global since. Kept for the error re-render too.
    let expected_revision = match form.take_revision() {
        Ok(revision) => revision,
        Err(msg) => return toast_only_error(&msg),
    };

    let form_for_error = form.clone();
    let submitted_action = action.clone();

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
        expected_revision,
    };

    let result = spawn_request_blocking(move || update_global_document_blocking(params)).await;

    match result {
        Ok(Ok(_)) => htmx_redirect(&paths::global(&slug)),
        Ok(Err(e)) => match e {
            ServiceError::AccessDenied(_) => {
                forbidden(&state, "You don't have permission to update this global")
            }
            ServiceError::Validation(ref ve) => {
                let render = FormRender {
                    state: &state,
                    def: &def,
                    form: &form_for_error,
                    auth_user: auth_user.as_ref(),
                    submitted_locale: submitted_locale.as_deref(),
                    hx,
                    revision: expected_revision,
                    conflict: None,
                };

                render_validation_error(&render, ve).await
            }
            ServiceError::Conflict(_) if !conflict_keeps_form(&submitted_action) => {
                unpublish_conflict_response(&state, ui_locale_of(auth_user.as_ref()))
            }
            ServiceError::Conflict(conflict) => {
                let render = FormRender {
                    state: &state,
                    def: &def,
                    form: &form_for_error,
                    auth_user: auth_user.as_ref(),
                    submitted_locale: submitted_locale.as_deref(),
                    hx,
                    revision: Some(conflict.current),
                    conflict: Some(RevisionConflictNotice::new(
                        paths::global(&slug),
                        submitted_action,
                    )),
                };

                render_revision_conflict(&render).await
            }
            // A hook abort, a dangling reference, a lock-retry exhaustion:
            // toast it over the form exactly as the collection edit form does,
            // instead of redirecting back as if the save had gone through.
            other => write_error_response(
                &state,
                ui_locale_of(auth_user.as_ref()),
                "Global update",
                other,
            ),
        },
        Err(e) => {
            error!("Global update task error: {}", e);
            redirect_response(&paths::global(&slug))
        }
    }
}
