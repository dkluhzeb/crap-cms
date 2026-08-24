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
            BasePageContext, Breadcrumb, CollectionContext, CollectionPermissions, PageMeta,
            PageType,
            field::{BaseFieldData, ConditionData, FieldContext, TextField, ValidationAttrs},
            page::collections::{CollectionCreatePage, UploadFormContext},
        },
        handlers::shared::{
            EnrichOptions, apply_display_conditions, build_field_contexts,
            build_locale_template_data, check_access_or_forbid, collection_base,
            enrich_field_contexts, extract_editor_locale, forbidden, get_user_doc,
            is_non_default_locale, render_page, require_collection, split_sidebar_fields,
        },
    },
    core::{AuthUser, Claims, CollectionDefinition, DocumentFields},
    db::AccessResult,
    hooks::ConditionContext,
};

/// Build, enrich, and split the field contexts for the create form.
fn prepare_create_fields(
    state: &AdminState,
    def: &CollectionDefinition,
    editor_locale: Option<&str>,
    auth_user: Option<&Extension<AuthUser>>,
) -> (Vec<FieldContext>, Vec<FieldContext>) {
    let non_default_locale = is_non_default_locale(state, editor_locale);
    let empty: HashMap<String, String> = HashMap::new();

    let mut fields = build_field_contexts(&def.fields, &empty, &empty, true, non_default_locale);

    enrich_field_contexts(
        &mut fields,
        &def.fields,
        &DocumentFields::new(),
        state,
        &EnrichOptions::builder(&HashMap::new())
            .filter_hidden(true)
            .non_default_locale(non_default_locale)
            .user(get_user_doc(auth_user))
            .build(),
    );

    let cond_ctx = ConditionContext {
        collection: &def.slug,
        operation: "create",
        user: get_user_doc(auth_user),
        ui_locale: auth_user.map(|Extension(au)| au.ui_locale.as_str()),
        locale: editor_locale,
        options: None,
    };

    apply_display_conditions(
        &mut fields,
        &def.fields,
        &json!({}),
        &state.hook_runner,
        true,
        &cond_ctx,
    );

    if def.is_auth_collection() {
        fields.push(FieldContext::Password(TextField {
            base: BaseFieldData {
                name: "password".to_string(),
                field_name: "password".to_string(),
                label: "password".to_string(),
                required: true,
                value: Value::String(String::new()),
                placeholder: None,
                description: Some("set_password_description".to_string()),
                readonly: false,
                localized: false,
                locale_locked: false,
                position: None,
                template: None,
                extra: serde_json::Map::new(),
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
    let def = match require_collection(&state, &slug) {
        Ok(d) => d,
        Err(resp) => return *resp,
    };

    match check_access_or_forbid(
        &state,
        def.access.create.as_ref(),
        auth_user.as_ref(),
        None,
        None,
        "create",
        &slug,
    ) {
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
        prepare_create_fields(&state, &def, editor_locale.as_deref(), auth_user.as_ref());
    let (_locale_ctx, locale_data) = build_locale_template_data(&state, editor_locale.as_deref());

    let claims_ref = claims.as_ref().map(|Extension(c)| c);

    let mut breadcrumbs = collection_base(&def, &slug);
    breadcrumbs.push(Breadcrumb::current("create_name").with_name(def.singular_name()));

    let base = BasePageContext::for_handler(
        &state,
        claims_ref,
        auth_user.as_ref(),
        PageMeta::new(PageType::CollectionCreate, "create_name")
            .with_title_name(def.singular_name()),
    )
    .with_editor_locale(editor_locale.as_deref(), &state)
    .with_breadcrumbs(breadcrumbs);

    let upload = def
        .is_upload_collection()
        .then(|| upload_accept_context(&def));

    let perms = CollectionPermissions::for_user(&state, &def, auth_user.as_ref());

    let ctx = CollectionCreatePage {
        base,
        collection: CollectionContext::from_def(&def),
        perms,
        fields: main_fields,
        sidebar_fields,
        editing: false,
        has_drafts: def.has_drafts(),
        locale_data,
        upload,
    };

    render_page(&state, "collections/edit", &ctx)
}

#[cfg(test)]
mod tests {
    use crate::core::upload::CollectionUpload;

    use super::*;

    #[test]
    fn accept_joins_declared_mime_types() {
        let mut def = CollectionDefinition::new("media");
        def.upload = Some(CollectionUpload {
            mime_types: vec!["image/png".into(), "image/jpeg".into()],
            ..Default::default()
        });
        assert_eq!(
            upload_accept_context(&def).accept.as_deref(),
            Some("image/png,image/jpeg")
        );
    }

    #[test]
    fn accept_is_none_without_upload_or_with_no_mime_types() {
        let no_upload = CollectionDefinition::new("posts");
        assert!(upload_accept_context(&no_upload).accept.is_none());

        let mut empty = CollectionDefinition::new("media");
        empty.upload = Some(CollectionUpload::default()); // no mime types declared
        assert!(upload_accept_context(&empty).accept.is_none());
    }
}
