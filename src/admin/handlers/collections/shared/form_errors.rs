//! Form error rendering — unified handling for upload errors and validation errors.

use std::collections::HashMap;

use axum::{Extension, response::Response};
use serde_json::{Map, Value, json};

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, CollectionContext, DocumentRef, PageMeta, PageType,
            page::collections::CollectionFormErrorPage,
        },
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts, enrich_field_contexts,
            page_with_toast, split_sidebar_fields, translate_validation_errors,
        },
    },
    core::{
        auth::AuthUser, collection::CollectionDefinition, field::FieldDefinition,
        validate::ValidationError,
    },
};

/// Collect hidden upload field values from form data for re-rendering after validation errors.
pub(in crate::admin::handlers::collections) fn collect_upload_hidden_fields(
    fields: &[FieldDefinition],
    form_data: &HashMap<String, String>,
) -> Value {
    let hidden_fields: Vec<Value> = fields
        .iter()
        .filter(|f| f.admin.hidden)
        .filter_map(|f| {
            form_data
                .get(&f.name)
                .map(|v| json!({"name": &f.name, "value": v}))
        })
        .collect();

    json!(hidden_fields)
}

/// Parameters for re-rendering a form with errors.
pub(in crate::admin::handlers::collections) struct FormErrorParams<'a> {
    pub state: &'a AdminState,
    pub def: &'a CollectionDefinition,
    pub form_data: &'a HashMap<String, String>,
    pub join_data: &'a HashMap<String, Value>,
    pub error_map: &'a HashMap<String, String>,
    pub doc_id: Option<&'a str>,
    pub auth_user: &'a Option<Extension<AuthUser>>,
    pub toast_msg: &'a str,
}

/// Build and render the form with an error toast. Handles both create (`doc_id = None`)
/// and edit (`doc_id = Some(id)`) modes, including upload hidden field preservation.
pub(in crate::admin::handlers::collections) fn render_form_with_error(
    p: &FormErrorParams,
) -> Response {
    let mut fields = build_field_contexts(&p.def.fields, p.form_data, p.error_map, true, false);

    let mut enrich_opts = EnrichOptions::builder(p.error_map).filter_hidden(true);

    if let Some(id) = p.doc_id {
        enrich_opts = enrich_opts.doc_id(id);
    }

    enrich_field_contexts(
        &mut fields,
        &p.def.fields,
        p.join_data,
        p.state,
        &enrich_opts.build(),
    );

    let form_json = if p.doc_id.is_some() {
        json!(
            p.form_data
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect::<Map<String, Value>>()
        )
    } else {
        json!({})
    };

    apply_display_conditions(
        &mut fields,
        &p.def.fields,
        &form_json,
        &p.state.hook_runner,
        true,
    );

    let (main_fields, sidebar_fields) = split_sidebar_fields(fields);

    let editing = p.doc_id.is_some();
    let (page_type, page_key) = if editing {
        (PageType::CollectionEdit, "edit_name")
    } else {
        (PageType::CollectionCreate, "create_name")
    };

    let base = BasePageContext::for_handler(
        p.state,
        None,
        p.auth_user,
        PageMeta::new(page_type, page_key).with_title_name(p.def.singular_name()),
    );

    let upload_hidden_fields = (editing && p.def.is_upload_collection()).then(|| {
        let value = collect_upload_hidden_fields(&p.def.fields, p.form_data);
        match value {
            Value::Array(arr) => arr,
            _ => Vec::new(),
        }
    });

    let ctx = CollectionFormErrorPage {
        base,
        collection: CollectionContext::from_def(p.def),
        document: p.doc_id.map(DocumentRef::stub),
        fields: main_fields,
        sidebar_fields,
        editing,
        has_drafts: p.def.has_drafts(),
        upload_hidden_fields,
    };

    page_with_toast(p.state, "collections/edit", &ctx, p.toast_msg)
}

/// Render the upload error page (create mode).
pub(in crate::admin::handlers::collections) fn render_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    auth_user: &Option<Extension<AuthUser>>,
    err_msg: &str,
) -> Response {
    render_form_with_error(&FormErrorParams {
        state,
        def,
        form_data,
        join_data: &HashMap::new(),
        error_map: &HashMap::new(),
        doc_id: None,
        auth_user,
        toast_msg: err_msg,
    })
}

/// Render the upload error page (edit mode).
pub(in crate::admin::handlers::collections) fn render_edit_upload_error(
    state: &AdminState,
    def: &CollectionDefinition,
    form_data: &HashMap<String, String>,
    id: &str,
    auth_user: &Option<Extension<AuthUser>>,
    err_msg: &str,
) -> Response {
    render_form_with_error(&FormErrorParams {
        state,
        def,
        form_data,
        join_data: &HashMap::new(),
        error_map: &HashMap::new(),
        doc_id: Some(id),
        auth_user,
        toast_msg: err_msg,
    })
}

/// Re-render the form with validation errors (works for both create and edit).
pub(in crate::admin::handlers::collections) fn render_form_validation_errors(
    state: &AdminState,
    def: &CollectionDefinition,
    doc_id: Option<&str>,
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

    render_form_with_error(&FormErrorParams {
        state,
        def,
        form_data,
        join_data,
        error_map: &error_map,
        doc_id,
        auth_user,
        toast_msg,
    })
}
