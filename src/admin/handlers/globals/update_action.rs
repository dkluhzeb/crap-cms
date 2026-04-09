use std::collections::HashMap;

use axum::{
    Extension,
    extract::{Form, Path, State},
    response::Response,
};
use serde_json::{Value, json};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::{
            forms::{extract_join_data_from_form, transform_select_has_many},
            shared::{
                EnrichOptions, apply_display_conditions, build_field_contexts,
                enrich_field_contexts, forbidden, get_event_user, get_user_doc, html_with_toast,
                htmx_redirect, redirect_response, split_sidebar_fields,
                translate_validation_errors,
            },
        },
    },
    core::{
        Document,
        auth::AuthUser,
        collection::{GlobalDefinition, Hooks},
        event::{EventOperation, EventTarget},
        validate::ValidationError,
    },
    db::{
        DbPool,
        query::{LocaleContext, LocaleMode},
    },
    hooks::{HookRunner, lifecycle::PublishEventInput},
    service::{self, ServiceError},
};

/// Parameters for the blocking global-update task.
struct UpdateParams {
    pool: DbPool,
    runner: HookRunner,
    slug: String,
    def: GlobalDefinition,
    form_data: HashMap<String, String>,
    join_data: HashMap<String, Value>,
    locale_ctx: Option<LocaleContext>,
    locale: Option<String>,
    draft: bool,
    user_doc: Option<Document>,
    ui_locale: Option<String>,
    action: String,
}

/// Execute the global update (or unpublish) inside a blocking task.
fn execute_update(
    params: UpdateParams,
) -> Result<(Document, HashMap<String, Value>), ServiceError> {
    if params.action == "unpublish" && params.def.has_versions() {
        let doc = service::unpublish_global_document(
            &params.pool,
            &params.runner,
            &params.slug,
            &params.def,
            params.user_doc.as_ref(),
        )?;

        Ok((doc, HashMap::new()))
    } else {
        service::update_global_document(
            &params.pool,
            &params.runner,
            &params.slug,
            &params.def,
            service::WriteInput::builder(params.form_data, &params.join_data)
                .locale_ctx(params.locale_ctx.as_ref())
                .locale(params.locale)
                .draft(params.draft)
                .ui_locale(params.ui_locale)
                .build(),
            params.user_doc.as_ref(),
        )
    }
}

/// Publish a global update event.
fn publish_update_event(
    state: &AdminState,
    hooks: &Hooks,
    def: &GlobalDefinition,
    slug: &str,
    doc: &Document,
    auth_user: &Option<Extension<AuthUser>>,
) {
    state.hook_runner.publish_event(
        &state.event_bus,
        hooks,
        def.live.as_ref(),
        PublishEventInput::builder(EventTarget::Global, EventOperation::Update)
            .collection(slug.to_string())
            .document_id(doc.id.clone())
            .data(doc.fields.clone())
            .edited_by(get_event_user(auth_user))
            .build(),
    );
}

/// Build the validation error response with re-rendered form fields.
fn render_validation_error(
    state: &AdminState,
    def: &GlobalDefinition,
    form_data: &HashMap<String, String>,
    join_data: &HashMap<String, Value>,
    ve: &ValidationError,
    auth_user: &Option<Extension<AuthUser>>,
) -> Response {
    let locale = auth_user
        .as_ref()
        .map(|Extension(au)| au.ui_locale.as_str())
        .unwrap_or("en");

    let error_map = translate_validation_errors(ve, &state.translations, locale);
    let toast_msg = state.translations.get(locale, "validation.error_summary");

    let mut fields = build_field_contexts(&def.fields, form_data, &error_map, false, false);

    let doc_fields: HashMap<String, Value> = form_data
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .chain(join_data.iter().map(|(k, v)| (k.clone(), v.clone())))
        .collect();

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

    let data = ContextBuilder::new(state, None)
        .locale_from_auth(auth_user)
        .filter_nav_by_access(state, auth_user)
        .page(PageType::GlobalEdit, def.display_name())
        .global_def(def)
        .fields(main_fields)
        .set("sidebar_fields", json!(sidebar_fields))
        .build();

    html_with_toast(state, "globals/edit", &data, toast_msg)
}

/// POST /admin/globals/{slug} — update a global
pub async fn update_action(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    auth_user: Option<Extension<AuthUser>>,
    Form(mut form_data): Form<HashMap<String, String>>,
) -> Response {
    let def = match state.registry.get_global(&slug) {
        Some(d) => d.clone(),
        None => return redirect_response("/admin"),
    };

    let action = form_data.remove("_action").unwrap_or_default();
    let form_locale = form_data.remove("_locale");
    let locale_ctx =
        LocaleContext::from_locale_string(form_locale.as_deref(), &state.config.locale)
            .unwrap_or(None);

    // Field write access is now checked inside service::update_global_core.

    transform_select_has_many(&mut form_data, &def.fields);
    let join_data = extract_join_data_from_form(&form_data, &def.fields);

    let locale = locale_ctx.as_ref().and_then(|ctx| match &ctx.mode {
        LocaleMode::Single(l) => Some(l.clone()),
        _ => None,
    });

    let params = UpdateParams {
        pool: state.pool.clone(),
        runner: state.hook_runner.clone(),
        slug: slug.clone(),
        def: def.clone(),
        form_data: form_data.clone(),
        join_data: join_data.clone(),
        locale_ctx,
        locale,
        draft: action == "save_draft",
        user_doc: get_user_doc(&auth_user).cloned(),
        ui_locale: auth_user.as_ref().map(|Extension(au)| au.ui_locale.clone()),
        action,
    };

    let result = task::spawn_blocking(move || execute_update(params)).await;

    match result {
        Ok(Ok((doc, _))) => {
            publish_update_event(&state, &def.hooks, &def, &slug, &doc, &auth_user);
            htmx_redirect(&format!("/admin/globals/{}", slug))
        }
        Ok(Err(e)) => match e {
            ServiceError::AccessDenied(_) => {
                forbidden(&state, "You don't have permission to update this global")
            }
            ServiceError::Validation(ref ve) => {
                render_validation_error(&state, &def, &form_data, &join_data, ve, &auth_user)
            }
            other => {
                error!("Global update error: {}", other);
                redirect_response(&format!("/admin/globals/{}", slug))
            }
        },
        Err(e) => {
            error!("Global update task error: {}", e);
            redirect_response(&format!("/admin/globals/{}", slug))
        }
    }
}
