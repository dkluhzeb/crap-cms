use std::collections::HashMap;

use axum::{
    Extension,
    extract::{Path, State},
    http::HeaderMap,
    response::Response,
};
use serde_json::{Value, json};

use crate::{
    admin::{
        AdminState,
        context::{
            BasePageContext, Breadcrumb, CollectionContext, PageMeta, PageType,
            field::{BaseFieldData, ConditionData, FieldContext, TextField, ValidationAttrs},
            page::collections::{CollectionCreatePage, UploadFormContext},
        },
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, check_access_or_forbid, enrich_field_contexts,
            extract_editor_locale, forbidden, is_non_default_locale, not_found, paths, render_page,
            split_sidebar_fields,
        },
    },
    core::{AuthUser, Claims, CollectionDefinition},
    db::AccessResult,
};

/// Build, enrich, and split the field contexts for the create form.
fn prepare_create_fields(
    state: &AdminState,
    def: &CollectionDefinition,
    editor_locale: Option<&str>,
) -> (Vec<FieldContext>, Vec<FieldContext>) {
    let non_default_locale = is_non_default_locale(state, editor_locale);
    let empty: HashMap<String, String> = HashMap::new();

    let mut fields = build_field_contexts(
        &def.fields,
        &empty,
        &HashMap::new(),
        true,
        non_default_locale,
    );

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &HashMap::new(),
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .build(),
    );

    apply_display_conditions(
        &mut fields,
        &def.fields,
        &json!({}),
        &state.hook_runner,
        true,
    );

    if def.is_auth_collection() {
        fields.push(FieldContext::Password(TextField {
            base: BaseFieldData {
                name: "password".to_string(),
                label: "password".to_string(),
                required: true,
                value: Value::String(String::new()),
                placeholder: None,
                description: Some("set_password_description".to_string()),
                readonly: false,
                localized: false,
                locale_locked: false,
                position: None,
                error: None,
                validation: ValidationAttrs::default(),
                condition: ConditionData::default(),
            },
            has_many: None,
            tags: None,
        }));
    }

    split_sidebar_fields(fields)
}

/// Build the upload accept context for upload collection create forms.
fn upload_accept_context(def: &CollectionDefinition) -> UploadFormContext {
    UploadFormContext {
        accept: def
            .upload
            .as_ref()
            .filter(|u| !u.mime_types.is_empty())
            .map(|u| u.mime_types.join(",")),
        ..UploadFormContext::default()
    }
}

/// GET /admin/collections/{slug}/create — show create form
pub async fn create_form(
    State(state): State<AdminState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    claims: Option<Extension<Claims>>,
    auth_user: Option<Extension<AuthUser>>,
) -> Response {
    let def = match state.registry.get_collection(&slug) {
        Some(d) => d.clone(),
        None => {
            return not_found(&state, &format!("Collection '{}' not found", slug));
        }
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

    let editor_locale = extract_editor_locale(&headers, &state.config.locale);
    let (main_fields, sidebar_fields) =
        prepare_create_fields(&state, &def, editor_locale.as_deref());
    let (_locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let breadcrumbs = vec![
        Breadcrumb::link("collections", "/admin/collections"),
        Breadcrumb::link(def.display_name(), paths::collection(&slug)),
        Breadcrumb::current("create_name").with_name(def.singular_name()),
    ];

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        &auth_user,
        PageMeta::new(PageType::CollectionCreate, "create_name")
            .with_title_name(def.singular_name()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let upload = def
        .is_upload_collection()
        .then(|| upload_accept_context(&def));

    let ctx = CollectionCreatePage {
        base,
        collection: CollectionContext::from_def(&def),
        fields: main_fields,
        sidebar_fields,
        editing: false,
        has_drafts: def.has_drafts(),
        locale_data,
        upload,
    };

    render_page(&state, "collections/edit", &ctx)
}
